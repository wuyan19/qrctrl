# CLAUDE.md

本文件为 Claude Code (claude.ai/code) 在本仓库中工作时提供指导。

## 项目概述

qrctrl 是一个用 Rust 编写的跨平台远程输入工具。手机扫码连接后，将手机上输入的文字（语音转写、键盘打字、emoji 等）通过 WebSocket 发送到 PC，PC 端用 enigo 注入到当前焦点窗口。命名取「QR + Control」，未来扩展到快捷键、鼠标等控制类指令。

## 构建与运行命令

```shell
cargo build                    # 调试构建
cargo build --release          # 发布构建（启用 LTO + strip）
cargo run                      # 以调试模式运行
cargo install --path .         # 本地安装
```

测试：`cargo test`

## 架构

二进制入口（`src/main.rs`）启动 axum HTTP + WebSocket 服务器，监听 `0.0.0.0:8080`，终端打印二维码供手机扫码。

**模块职责：**

- **`main.rs`** — 程序入口。生成 token、初始化 enigo、构造 AppState、装配 axum 路由（`/` 返回静态 HTML、`/ws` 升级 WebSocket）、绑定端口、生成扫码 URL、调用 qr 模块渲染二维码到终端。token 嵌入 URL 查询参数，未带 token 或 token 不匹配时 WebSocket 升级返回 401。

- **`token.rs`** — 生成一次性随机 token（字母数字组合），用于扫码 URL 鉴权。

- **`state.rs`** — `AppState` 结构体，包含 `token: String` 和 `enigo: Arc<Mutex<Enigo>>`。`#[derive(Clone)]` 满足 axum State 的 Clone + Send + Sync 要求。enigo 用 `Arc<Mutex<...>>` 包裹，因为 macOS 上 Enigo 实现了 Send 但没实现 Sync，必须加 Mutex 才能跨线程共享（详见 `tests/send_sync_invariants.rs`）。

- **`ws.rs`** — WebSocket 处理。`ws_handler` 校验 token，通过后升级连接。`handle_socket` 循环接收消息：文本消息通过 `tokio::task::spawn_blocking` 丢到阻塞线程池调用 enigo（enigo 的 `text()` 是阻塞调用，直接在 executor 上跑会卡异步运行时）；Close 或 Err 退出循环。

- **`inject.rs`** — `inject_text(enigo, text)` 锁住 Mutex 后调用 `Enigo::text()` 注入文本。这是 enigo 的 Unicode 注入路径，**不依赖当前键盘布局或输入法状态**，跨平台一致。

- **`net.rs`** — `get_local_ipv4()` 找一个非 loopback 的局域网 IPv4 地址，用于生成扫码 URL。取不到时 main.rs 回退到 localhost。

- **`qr.rs`** — `render_qr_to_terminal(text)` 用 Unicode 半角块字符（`█▀▄`）把二维码渲染到终端，每字符表示 2×2 个 QR 模块，扫描体验最好。

## 金丝雀规则

**语言要求**：所有生成的文档、向用户提出的问题、代码注释，都必须使用中文。仅在确实必要时才使用英文，例如：代码标识符、命令行参数、技术专有名词等本身为英文的内容。

## 关键设计细节

- **传输层**：axum 0.8 提供 HTTP（`/` 返回静态 `index.html`，用 `include_str!` 编译期内联）和 WebSocket（`/ws`）。前端在 `static/index.html` 中实现，无前端构建链。
- **鉴权**：扫码 URL 含 `?t=<token>`，前端解析后用于 WebSocket 升级请求。无 token 或 token 不匹配返回 401。token 在每次启动时随机生成。
- **enigo 线程模型**：`Arc<Mutex<Enigo>>` + `spawn_blocking`。在 macOS 上 Enigo 不实现 Sync（CoreFoundation 句柄不是 Sync），所以必须 Mutex 包裹；在 Windows/Linux 上 Enigo 实际实现了 Send + Sync，但代码统一用 Mutex 简化跨平台逻辑。
- **平台权限**：macOS 首次启动需要授予「辅助功能」权限（系统设置 → 隐私与安全性），并允许防火墙入站（监听 0.0.0.0:8080 时弹窗）。Windows/Linux 无需特殊权限（Linux 需 X11 + libxtst）。每次重新编译后 macOS 可能需要重新授权（TCC 按签名指纹记录权限）。
- **文本注入路径**：`Enigo::text()` 在 macOS 用 `CGEventCreateUnicodeString`、Windows 用 `SendInput` Unicode path、Linux 用 XTest 的 Unicode 键码。三种路径都绕过键盘布局，所以中文/emoji 直接出字，不需要切换输入法。
- **断线重连**：前端实现指数退避重连（1s → 2s → 4s → ... 上限 10s），后端无状态。
- **平台条件编译**：当前代码无 `#[cfg]` 分支，enigo 库自己处理平台差异。Linux 上 enigo 需要 `libxtst-dev`、`libx11-dev`、`libxdo-dev`（CI 中已配置）。
- **错误处理**：启动期错误（端口绑定失败、enigo 初始化失败）用 `expect` 直接 panic；运行时错误（WebSocket 错误、注入失败）用 `eprintln!` 记录后继续，不退出服务器。

## 扩展计划

当前只支持文本注入。未来计划扩展（详见 `docs/research.md`）：

- 快捷键序列（如 `Cmd+Space`、`Win+R`）
- 鼠标移动和点击（绝对 / 相对坐标）
- 滚轮
- 协议从纯文本升级为 JSON 指令，向后兼容
- 安全考虑：快捷键白名单或二次确认机制
