# qrctrl 架构评审与改进路线

> 评审日期：2026-06-18
> 评审范围：`src/` 全部 11 个 Rust 模块（约 3249 行）+ `static/` 前端（约 3119 行）
> 评审版本：0.8.0

本文档记录对 qrctrl 项目的系统架构与技术栈的深度分析，包括优点、缺点和具体改进方案，作为后续重构与演进的依据。

---

## 一、架构总览

```
┌─────────────────────────────────────────────────────────┐
│  main thread: tao event loop (tray + QR window)         │ ← 必须主线程 (macOS)
│     ├─ tray icon / menu                                 │
│     └─ QR 弹窗 (softbuffer 软渲染)                       │
└──────────────┬──────────────────────────────────────────┘
               │ EventLoopProxy<UserEvent> + Arc<Notify>
┌──────────────▼──────────────────────────────────────────┐
│  server thread: tokio multi-thread runtime              │
│     └─ axum Router                                      │
│          ├─ GET  /            index.html (主题注入)      │
│          ├─ WS   /ws          指令通道 (键鼠/剪贴板/上传) │
│          ├─ POST /upload/:id  流式上传                   │
│          ├─ GET  /download/:id 流式下载                  │
│          └─ /config + /api/*  配置页 + 9 个 API          │
└─────────────────────────────────────────────────────────┘
```

**核心数据流**：手机浏览器 → WebSocket（控制面，JSON）+ HTTP（数据面，流式二进制）→ `AppState`（Arc 共享）→ `spawn_blocking` → 系统调用（enigo/arboard）。

**模块规模**：

| 模块 | 职责 | 行数 |
|---|---|---|
| `inject.rs` | 纯函数封装 enigo（键鼠注入） | 93 |
| `token.rs` | token 生成/校验 | 91 |
| `qr.rs` | QR 渲染（终端 + 像素） | 120 |
| `state.rs` | 共享状态定义 | 44 |
| `clipboard.rs` | 剪贴板 + 纯函数编解码 | 303 |
| `net.rs` | 网卡枚举/过滤 | 306 |
| `file_transfer.rs` | 文件传输 + registry | 338 |
| `tray.rs` | GUI 事件循环 | 374 |
| `ws.rs` | WS 协议层 | 601 |
| `config.rs` | 配置 + 9 个 handler | 511 |
| `main.rs` | 启动装配 | 469 |

---

## 二、优点（保持这些）

### 1. 模块划分干净，单一职责清晰
没有"上帝模块"，最大的 `ws.rs` 也只承担 WS 协议职责。`inject.rs`/`qr.rs`/`token.rs` 是教科书式的薄封装层。

### 2. 纯函数与副作用分离
`clipboard.rs` 把阻塞的 arboard 调用和纯计算（`decode_bytes_to_rgba`/`encode_rgba_to_png_base64`）分开，让纯函数可单测。`file_transfer.rs` 的 `sanitize_filename`/`resolve_conflict`/`percent_encode_filename` 同理。

### 3. 跨平台约束处理到位
- macOS 主线程跑 event loop（NSApplication 约束）
- Windows GUI 子系统 + 智能挂载父 console
- 阻塞调用统一走 `spawn_blocking`（enigo/arboard/文件元数据）
- `restart_handler` 用 `QRCTRL_RESTART_CHILD` 环境变量解决端口竞争

### 4. 安全细节考虑周到
- `sanitize_filename` 显式拒绝 `ParentDir`（防 `../` 绕过）
- 强制 `Content-Type: application/json` 借 CORS preflight 做免费 CSRF 防御
- token 校验后路由才能访问
- 配置文件损坏改名备份而非 panic
- `inject_shortcut` 即使失败也释放修饰键，避免 Ctrl 卡住

### 5. 注释质量极高
几乎每个非显然决策都有"为什么这样做"的注释，远超一般个人项目。

---

## 三、缺点与改进点

### 🔴 缺点 1：`ws.rs` 存在严重的样板代码（最大问题）

`inject_key_cmd` / `inject_mouse_move_cmd` / `inject_mouse_button_cmd` / `inject_mouse_button_press_cmd` / `inject_mouse_button_release_cmd` / `inject_mouse_scroll_cmd` / `inject_copy_cmd` / `inject_paste_cmd` **8 个函数结构完全一致**，每个约 18 行，共 ~150 行重复代码。模式都是：

```rust
async fn xxx_cmd(enigo: &Arc<Mutex<Enigo>>, ...) -> String {
    let enigo = enigo.clone();
    let result = tokio::task::spawn_blocking(move || inject::xxx(&enigo, ...)).await;
    match result {
        Ok(Ok(())) => ok_json(),
        Ok(Err(e)) => { eprintln!(...); error_json("inject_failed") }
        Err(e)     => { eprintln!(...); error_json("internal") }
    }
}
```

**改进方案**：抽一个通用 helper，把 `Result<Result<(), String>, JoinError>` 收敛成 JSON。预计可砍掉 ~130 行。同样的样板也存在于 `dispatch` 的 `GetClipboardText`/`GetClipboardImage`/`GetFile` 三个分支。

### 🔴 缺点 2：鉴权逻辑重复 9 次

`config.rs` 里 9 个 handler 每个都以这段开头：

```rust
if q.t != state.token {
    return Err(StatusCode::UNAUTHORIZED)...
}
```

**改进方案**：用 axum 的 `FromRequestParts` extractor（自定义 `Authed` 标记类型），handler 签名变成 `async fn handler(_: Authed, State(s): State<AppState>) -> ...`。`ws_handler` 也可以复用。这是 axum 的惯用法。

### 🔴 缺点 3：`Mutex::lock().unwrap()` 散落多处（生产隐患）

`config.rs:167/191/192/260/308`、`ws.rs:74/219`、`main.rs:331` 都直接 `.unwrap()` 了 `std::sync::Mutex`。一旦任何持有锁的线程 panic，整个进程进入**不可恢复的 poison 状态**，所有后续 `.unwrap()` 全部 panic → 双击启动下静默崩溃。

`file_transfer.rs` 已经用了 `.expect("uploads lock poisoned")`（更诚实），但本质上同样是 panic。

**改进方案**：关键路径（theme/sensitivity 这类高频读取的）改用 `parking_lot::Mutex`（不 poison），或至少在 catch_unwind 后恢复。对商业软件这是 P1。

### 🟡 缺点 4：日志用 `println!`/`eprintln!`，无结构化

全项目 63 处 `println!`/`eprintln!`。问题：
- 无级别（debug/info/warn/error 混在一起）
- 无时间戳
- Windows GUI 子系统下 stdout 默认无效，`eprintln!` 到 `AttachConsole` 前的日志全丢
- 商业软件需要日志收集/审计时无能为力

**改进方案**：引入 `tracing` + `tracing-subscriber`，一行配置就支持 JSON 结构化日志、文件轮转、级别过滤。生产可观测性的基础设施。

### 🟡 缺点 5：单文件前端过于庞大

`index.html` 1839 行、`config.html` 1280 行，HTML/CSS/JS 全部 inline 在单文件里。问题：
- 无法 lint / format / type-check JS
- 无法复用组件（两个 HTML 里 CSS 主题、token 管理、fetch 封装大量重复）
- `include_str!` 编译期内联，改前端必须重编译
- 无 source map，生产排查 bug 困难

**改进方案**（渐进式）：
1. **最低成本**：把 JS/CSS 抽到 `static/js/app.js`、`static/css/style.css`，`index.html` 用 `<link>`/`<script src>` 引用，axum 用 `tower_http::services::ServeDir` 提供（依赖已装）。仍然零构建。
2. **进一步**：引入 Vite/esbuild 做打包 + tree-shaking。
3. **理想**：分离 `theme.css`/`api.js`/`ws.js`/`ui.js` 让两个页面共享。

### 🟡 缺点 6：缺少集成测试与错误路径覆盖

60+ 单元测试集中在纯函数和反序列化，但关键链路零集成测试：
- 没有 WebSocket → enigo/clipboard 的端到端测试（可 mock `inject` trait）
- 没有 HTTP upload/download 流程测试
- `dispatch` 函数（最复杂的业务逻辑）完全没测
- 错误码映射（`clipboard::error_code`）只测了正向

**改进方案**：定义一个 `InputBackend` trait（`fn inject_text`/`fn inject_key`/...），生产用 enigo 实现，测试用 mock 实现。这样 `dispatch` 可以单测，验证"收到 mouse_move 指令后调用注入的参数正确"。这是商业软件的基本要求。

### 🟡 缺点 7：HTTP 明文 + 短 token，安全模型薄弱

- 明文 HTTP，token 在 URL query string 里（会被浏览器历史、代理日志、Referer 泄露）
- token 是 16 位 hex（64 bit），局域网内虽够抗枚举，但任何能嗅探流量的人都能拿到 token（如公共 WiFi、被入侵的同一网段机器）
- 文件传输内容完全明文
- 没有 HTTPS/TLS 选项（README 路线图提到但未实现）

**改进方案**：
1. 短期：token 从 query 移到 `Authorization` header 或 `Sec-WebSocket-Protocol`（WS 场景）
2. 中期：用 `rustls` + 自签证书提供 HTTPS（首次扫码有警告，但可用 `mkcert` 或文档引导装根证书）
3. 长期：mTLS 双向认证，或基于二维码本身承载密钥做端到端加密

### 🟡 缺点 8：配置 live-apply 机制割裂

`theme` 和 `mouse_sensitivity` 走 live-apply（改 state + 写文件），其他字段走"写文件+重启"。这导致：
- `AppState` 里 `theme`/`mouse_sensitivity` 必须用 `Arc<Mutex<>>`，而其他字段是不可变 `String`/`u16`，类型不统一
- `set_theme_handler` 里 `load() → 改字段 → save()` 与 `set_config_handler` 的全量 save 是两条路径，容易出 bug
- 新增一个 live-apply 字段要改 4 处

**改进方案**：把可变运行时配置收敛成一个 `RuntimeConfig { theme, mouse_sensitivity, ... }`，用 `Arc<ArcSwap<RuntimeConfig>>` 无锁原子替换。所有字段统一一个心智模型。

### 🟢 缺点 9：`tray.rs` 的事件处理是巨型 match

`event_loop.run` 的 closure 里塞了 6 种 `Event` 分支，每个分支又有嵌套 `if/else if`（菜单项匹配）。172 行单闭包，可读性一般。

**改进方案**：把菜单事件处理抽成 `fn handle_menu_event(id, state, qr_window, target) -> ControlFlow`。纯重构，不改行为。

### 🟢 缺点 10：缺少可观测性与运维特性

商业软件还需要：
- **指标**（metrics）：连接数、指令 QPS、传输字节数、错误率
- **健康检查**端点
- **崩溃报告**（sentry / minidump）
- **自动更新**（目前用户要手动换二进制）
- **多设备/多用户**（目前单 token 单会话，`handle_socket` 是 per-connection 但没有连接上限）

---

## 四、是否满足"商业软件"要求？

对照商业软件基线评分：

| 维度 | 现状 | 评分 |
|---|---|---|
| 功能完整性 | 核心功能齐备 | ⭐⭐⭐⭐⭐ |
| 代码质量 | 干净、注释好，但有样板 | ⭐⭐⭐⭐ |
| 测试覆盖 | 纯函数好，集成/错误路径缺 | ⭐⭐⭐ |
| 可观测性 | println，无结构化日志/指标 | ⭐⭐ |
| 安全性 | 明文 HTTP，token 在 URL | ⭐⭐ |
| 跨平台鲁棒性 | 处理细致，但 poison mutex 有隐患 | ⭐⭐⭐⭐ |
| 可维护性 | 模块清晰，但前端单文件难扩展 | ⭐⭐⭐ |
| 运维（更新/崩溃报告）| 无 | ⭐ |

**结论**：作为个人/小团队开源工具，完成度极高，工程质量优秀。但要达到商业软件（付费分发、企业部署）标准，最关键的三个差距是：
1. **安全**（HTTPS + token 不进 URL + 加密传输）—— 企业 IT 部门采购的硬门槛
2. **可观测性**（结构化日志 + 指标 + 崩溃报告）—— 没法定位线上问题
3. **测试**（端到端集成测试 + 后端可 mock）—— 没法保证回归质量

---

## 五、改进优先级

| 优先级 | 改进项 | 投入 | 收益 |
|---|---|---|---|
| **P0** | 抽 `ws.rs` 的样板 helper（砍 ~130 行）| 0.5 天 | 可读性+维护性大幅提升 |
| **P0** | `Mutex::unwrap()` → `parking_lot` 或 poison 恢复 | 0.5 天 | 消除静默崩溃风险 |
| **P0** | 引入 `tracing` 替换 println | 0.5 天 | 可观测性基础设施 |
| **P1** | axum `FromRequestParts` 抽鉴权 extractor | 0.5 天 | 消除 9 处重复 |
| **P1** | 定义 `InputBackend` trait + mock，补 dispatch 集成测试 | 1-2 天 | 关键链路回归保障 |
| **P1** | HTTPS（rustls + 自签/let's encrypt LAN）| 2-3 天 | 安全门槛达标 |
| **P2** | 前端拆 JS/CSS 到独立文件 | 1 天 | 前端可维护性 |
| **P2** | token 从 URL query 移到 header | 1 天 | 减少 token 泄露面 |
| **P3** | metrics + 崩溃报告 + 自动更新 | 3-5 天 | 企业级运维 |

如果只能做三件事：先做 P0 的三项（样板消除、mutex 修复、tracing），这三项加起来 1.5 天，能让代码质量、稳定性、可观测性同时上一个台阶，且零功能风险。

---

## 六、改进进度跟踪

| 改进项 | 状态 | 完成日期 | 备注 |
|---|---|---|---|
| P0-1: ws.rs 样板消除 | ✅ 已完成 | 2026-06-18 | 抽 `spawn_inject` / `spawn_block` helper，ws.rs 从 601 行降到 559 行，8 个 ~18 行样板函数压缩为 ~3 行调用 |
| P0-2: parking_lot 替换 std Mutex | ✅ 已完成 | 2026-06-18 | state/clipboard/file_transfer/inject/ws/config/main 全量迁移；消除所有 `.lock().unwrap()`/`.expect()` 的 poison panic 风险；同步更新 send_sync_invariants 测试 |
| P0-3: tracing 接入 | ✅ 已完成 | 2026-06-18 | Cargo.toml 加 `tracing` + `tracing-subscriber`(env-filter)；main.rs `init_logging()`；全量替换业务 println/eprintln 为结构化日志（保留面向用户的 banner println） |
| P1-1: axum 鉴权 extractor | ✅ 已完成 | 2026-06-18 | 新增 `Authed` extractor（`state.rs`，零字段 marker，校验 `?t=<token>`）；config(9)+file_transfer(2)+ws(1) 共 12 个 handler 去除重复 `if q.t != state.token` 样板；`ListDirQuery`/`CheckPortQuery` 去掉 `#[serde(flatten)] token` 冗余字段 |
| P1-2: InputBackend trait + 集成测试 | ✅ 已完成 | 2026-06-18 | 新增 `backend.rs`（`InputBackend` trait + `EnigoBackend` 生产实现 + `DynBackend` 类型别名 + 统一 `BackendError`）；`AppState` 用 `Arc<dyn InputBackend + Send + Sync>` 替换原 `enigo`+`clipboard` 两字段；`dispatch` + `spawn_inject`/`spawn_block` 全改走 trait；新增 8 个集成测试（`MockBackend` 记录调用、断言参数传递与错误收敛），测试总数 69→77 |
| P1-3: HTTPS | ⏳ 待开始 | | |
| P2-1: 前端拆分 | ⏳ 待开始 | | |
| P2-2: token 移出 URL | ⏳ 待开始 | | |
| P3: metrics + 崩溃报告 + 自动更新 | ⏳ 待开始 | | |

### P0 完成验证

- 编译：`cargo build` 零错误零警告
- 测试：`cargo test` **69 单元测试 + 3 Send/Sync 测试全部通过，0 失败**
- 行为不变：所有重构均为内部实现，对外协议（WS 消息格式、HTTP API、错误码）完全保持

### P1-1 / P1-2 完成验证

- 编译：`cargo build` 零错误零警告；`cargo clippy` 对新增代码（backend.rs / Authed / ws.rs 改动）零警告
- 测试：`cargo test` **77 单元测试 + 3 Send/Sync 测试全部通过，0 失败**（P1-2 新增 8 个 MockBackend 集成测试）
- 行为不变：`EnigoBackend` 方法体全部单行转发到 `inject::`/`clipboard::` 现有函数，零行为变更；`dispatch` 路由表不变
- 可测性：`dispatch` 的调度机制（backend 方法调用、参数传递、错误码收敛）现可通过 `MockBackend` 验证，不再依赖真实 OS
