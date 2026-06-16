# CLAUDE.md

本文件为 Claude Code (claude.ai/code) 在本仓库中工作时提供指导。

## 项目概述

qrctrl 是一个用 Rust 编写的跨平台远程输入工具。手机扫码连接后：

- **文本注入**（手机 → PC）：手机上输入的文字（语音转写、键盘打字、emoji 等）通过 WebSocket 发送，PC 端用 enigo 注入到当前焦点窗口。
- **双向剪贴板同步**：
  - PC → 手机：拉取 PC 剪贴板的文本/图片，在手机端拿到（文本写剪贴板或 textarea 兜底；图片弹预览模态）。
  - 手机 → PC 图片：手机端选图或粘贴截图，PC 用 arboard 写入剪贴板。
- **命令行配置**：可指定监听地址 / 端口 / 设备名。设备名用于手机端状态栏显示，区分多台被控 PC。

命名取「QR + Control」，未来扩展到快捷键、鼠标、任意文件等控制类指令。

## 构建与运行命令

```shell
cargo build                    # 调试构建
cargo build --release          # 发布构建（启用 LTO + strip）
cargo run                      # 以调试模式运行（默认 0.0.0.0:8080）
cargo run -- --port 9000 --name "工作 Mac"   # 传命令行参数
cargo install --path .         # 本地安装
```

CLI 参数：

| 参数 | 短 | 默认 | 说明 |
|---|---|---|---|
| `--addr` | `-a` | `0.0.0.0` | 监听地址 |
| `--port` | `-p` | `8080` | 监听端口 |
| `--name` | `-n` | 系统主机名 | 设备名称（手机端显示用） |
| `--save-dir` | — | `<下载目录>/qrctrl/` | 手机上传文件的保存目录 |
| `--max-size` | — | `10737418240`（10 GB） | 单文件大小上限（字节） |
| `--token` | — | 随机生成 | 固定 token（4-64 位 ASCII 字母数字）。提供后重启程序扫码 URL 不变，手机端刷新即可重连 |
| `--prefer-ip` | — | — | 偏好的 IP 子网前缀（如 `192.168.20`），多网卡时用来挑 QR 码用的 IP。不匹配时回退到全部候选 |
| `--help` | `-h` | — | 帮助 |
| `--version` | `-V` | — | 版本（来自 Cargo.toml） |

测试：`cargo test`

## 架构

二进制入口（`src/main.rs`）启动 axum HTTP + WebSocket 服务器，监听 `0.0.0.0:8080`（可用 `--addr` / `--port` 覆盖），终端打印二维码供手机扫码。

**模块职责：**

- **`main.rs`** — 程序入口。用 `clap::Parser`（derive 风格）解析 CLI 参数；`resolve_name()` 决定设备名（`--name` 优先 → 系统主机名 → 兜底 `"qrctrl"`）；计算扫码 URL（多网卡筛选 + `--prefer-ip`）；打印 banner（QR 码 + URL）；启动 server 线程（tokio runtime + axum）；主线程跑 `tray::run_tray_event_loop`（阻塞）。tray 退出 → `Notify::notify_waiters` → server graceful shutdown → `join` server 线程后退出。

- **`tray.rs`** — 系统托盘。tao event loop 主线程跑（macOS NSApplication 强制主线程），tray icon + 菜单（复制 URL / 显示二维码 / 退出）。QR 码窗口用 softbuffer 在 tao Window 上直接画像素，避免依赖系统图片查看器。tray-icon 在 `Event::NewEvents(StartCause::Init)` 内创建（避免过早创建 panic）。`Arc<Notify>` 与 server 线程协调退出。

- **`token.rs`** — 生成一次性随机 token（字母数字组合），用于扫码 URL 鉴权。

- **`state.rs`** — `AppState` 结构体，包含 `token: String`、`name: String`、`enigo: Arc<Mutex<Enigo>>`、`clipboard: ClipboardHandle`（即 `Arc<Mutex<arboard::Clipboard>>`）。`#[derive(Clone)]` 满足 axum State 的 Clone + Send + Sync 要求。enigo 和 clipboard 用**独立 Mutex**——两件事无逻辑互斥关系；共用会拖慢（注入长文本期间无法读剪贴板）。两者都用 `Arc<Mutex<...>>` 包裹，因为 macOS 上 Enigo/Clipboard 都实现了 Send 但没实现 Sync（详见 `tests/send_sync_invariants.rs`）。

- **`ws.rs`** — WebSocket 处理 + JSON 协议分发。`ws_handler` 校验 token；`handle_socket` 升级后**立即推送 `server_info`**（含设备名），然后进入循环接收消息（文本指令通过 `dispatch` 处理后用 `socket.send()` 回包，Close 或 Err 退出）；`dispatch` 反序列化 JSON Command 并路由到对应处理器，所有阻塞调用（enigo.text / arboard.read / arboard.write）都通过 `tokio::task::spawn_blocking` 丢到阻塞线程池。Command 枚举用 `#[serde(tag = "type", rename_all = "snake_case")]`，目前 variant：`Text` / `GetClipboardText` / `GetClipboardImage` / `SetClipboardImage` / `UploadStart` / `GetFile` / `Enter` / `Tab` / `Backspace` / `Copy` / `Paste` / `MouseMove` / `MouseClick` / `MousePress` / `MouseRelease` / `MouseScroll`。

- **`inject.rs`** — `inject_text(enigo, text)` 锁住 Mutex 后调用 `Enigo::text()` 注入文本。这是 enigo 的 Unicode 注入路径，**不依赖当前键盘布局或输入法状态**，跨平台一致。

- **`clipboard.rs`** — arboard 包装。`ClipboardHandle = Arc<Mutex<arboard::Clipboard>>` 类型别名；纯函数 `decode_bytes_to_rgba` / `encode_rgba_to_png_base64` / `decode_base64`（不接触 arboard，便于单测）；副作用函数 `read_text` / `read_image_png_base64` / `write_image_from_bytes`（必须 spawn_blocking 调用）。`read_image_png_base64` 内部**优先调 `cb.get().file_list()`**：用户从 Finder / 资源管理器 Cmd+C 复制图片文件时，剪贴板里是文件引用而不是位图，`get_image()` 返回的是系统占位图标；只有文件列表为空或文件不是图片时才降级到 `get_image()`。常量：`MAX_PIXELS = 40_000_000`（解码后内存上限）、`MAX_IMG_B64 = 10_000_000`（base64 字符串上限）。

- **`qr.rs`** — `render_qr_to_terminal(text)` 用 Unicode 半角块字符（`█▀▄`）把二维码渲染到终端，每字符表示 2×2 个 QR 模块，扫描体验最好。`render_qr_to_pixels(text, scale, border)` 把 QR 渲染成 `Vec<u32>`（XRGB8888），供 tray 模块的二维码窗口用 softbuffer 直接显示。

- **`net.rs`** — 局域网 IP 候选筛选，用于生成扫码 URL。`list_local_ipv4s()` 通过 `list_afinet_netifas()` 枚举所有网卡 IPv4，用 `is_likely_lan()` 保守过滤（排除 loopback 127/8、link-local 169.254/16、RFC 2544 / RFC 6815 benchmark 段 192.18/15 和 198.18/15、VirtualBox 默认 host-only 192.168.56/21），再用 `is_virtual_interface()` 按接口名排除 WSL / Docker / Hyper-V / VMware / VPN 隧道网卡。`filter_by_subnet(ips, prefix)` 按子网前缀过滤候选，没匹配时返回原列表（让用户看到所有可选项，而不是空）。取不到候选时 main.rs 回退到 localhost。

## 金丝雀规则

**语言要求**：所有生成的文档、向用户提出的问题、代码注释，都必须使用中文。仅在确实必要时才使用英文，例如：代码标识符、命令行参数、技术专有名词等本身为英文的内容。

## 关键设计细节

- **传输层**：axum 0.8 提供 HTTP（`/` 返回静态 `index.html`，用 `include_str!` 编译期内联）和 WebSocket（`/ws`）。前端在 `static/index.html` 中实现，无前端构建链。
- **鉴权**：扫码 URL 含 `?t=<token>`，前端解析后用于 WebSocket 升级请求。无 token 或 token 不匹配返回 401。token 在每次启动时随机生成。**设备名不进 URL**——避免二维码变复杂 + 防止意外泄露设备标识，name 只通过已认证的 WebSocket 推送。
- **协议**：所有 WebSocket 消息都是 JSON，没有纯文本路径。server 和 client 同包发布（HTML 编译进 binary），无历史兼容包袱。
  - 服务端 → 客户端首条消息：`{"type":"server_info","name":"..."}`，前端用于状态栏显示。
  - 请求字段：`{"type":"<cmd>", ...}`；响应类型：`ok` / `clipboard_text` / `clipboard_image` / `empty` / `error`（含 `code`）。
- **CLI 解析**：`clap = "4"` 的 derive 模式。`Cli` 结构体含 `addr: String`、`port: u16`、`name: Option<String>`，版本号从 `Cargo.toml` 自动取。`hostname` crate 取系统主机名作为 name 默认值。
- **enigo 线程模型**：`Arc<Mutex<Enigo>>` + `spawn_blocking`。在 macOS 上 Enigo 不实现 Sync（CoreFoundation 句柄不是 Sync），所以必须 Mutex 包裹；在 Windows/Linux 上 Enigo 实际实现了 Send + Sync，但代码统一用 Mutex 简化跨平台逻辑。
- **arboard 线程模型**：与 enigo 同样模式，`Arc<Mutex<arboard::Clipboard>>` + `spawn_blocking`。macOS 上 Clipboard 同样仅 Send 不 Sync。
- **平台权限**：macOS 首次启动需要授予「辅助功能」权限（系统设置 → 隐私与安全性），并允许防火墙入站（监听 0.0.0.0:8080 时弹窗）。Windows/Linux 无需特殊权限（Linux 需 X11 + libxtst）。每次重新编译后 macOS 可能需要重新授权（TCC 按签名指纹记录权限）。
- **文本注入路径**：`Enigo::text()` 在 macOS 用 `CGEventCreateUnicodeString`、Windows 用 `SendInput` Unicode path、Linux 用 XTest 的 Unicode 键码。三种路径都绕过键盘布局，所以中文/emoji 直接出字，不需要切换输入法。
- **图片传输**：手机 ↔ PC 之间用 base64 over JSON。前端 `<input type="file" accept="image/*">` 选图 + document 级 `paste` 监听（截图直接粘贴）；后端用 `image` crate 解码任意格式 → RGBA8 → 像素总数校验 → arboard 写入或 PNG 编码。
- **自动发送**：前端 checkbox 开关，`input` 事件后停顿 600ms 自动发送；用 `compositionstart` / `compositionend` 跟踪 IME 状态，中文拼音选词期间不触发。手动 Enter / 发送按钮 / 清空按钮都会取消 pending timer。
- **大小限制**：文本 100 KB（超出截断，仍发送）；图片 base64 原文 10 MB（超出 `too_large`）；图片像素总数 ≤ 4000 万（防 RGBA 内存爆炸）。axum WebSocket 默认 `max_message_size = 64 MB`，无需调整。
- **断线重连**：前端实现指数退避重连（1s → 2s → 4s → ... 上限 10s），后端无状态。
- **平台条件编译**：当前代码无 `#[cfg]` 分支，arboard / enigo / hostname 库自己处理平台差异。Linux 上 enigo 需要 `libxtst-dev`、`libx11-dev`、`libxdo-dev`（CI 中已配置）。Linux 上 tray-icon / tao 还需要 `libgtk-3-dev`、`libayatana-appindicator3-dev`（或老版 `libappindicator3-dev`）。
- **系统托盘**：tray-icon + tao + softbuffer 实现。线程模型：主线程跑 tao event loop（macOS NSApplication 强制），server 的 tokio runtime 跑在子线程；退出通过 `tokio::sync::Notify` 协调，axum `with_graceful_shutdown` 让 server 优雅关闭（避免上传半成品文件残留）。binary 增加 ~400 KB（Windows release）。
- **Windows subsystem**：release 模式编译为 GUI subsystem（`#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]`），双击 exe 不弹 cmd 黑窗，关任何终端都不杀进程。代价：windows subsystem 下 stdout/stderr 默认无效，所以在 `main()` 第一行调 `attach_parent_console()`——从 PowerShell/cmd 启动时 `AttachConsole(ATTACH_PARENT_PROCESS)` + 重绑 `STD_OUTPUT_HANDLE`/`STD_ERROR_HANDLE` 到 `CONOUT$`，println!/eprintln! 正常输出；双击启动时无父 console，`AttachConsole` 失败，静默跳过。debug 模式保留 console subsystem（`debug_assertions` 为真），开发时 panic backtrace 可见。
- **错误处理**：启动期错误（端口绑定失败、enigo/clipboard 初始化失败）用 `expect` 直接 panic；运行时错误（WebSocket 错误、注入失败、剪贴板错误）用 `eprintln!` 记录后继续，不退出服务器。`CbError` 映射为协议错误码字符串（`empty` / `clipboard_busy` / `decode_failed` / `too_large` / `internal`）。

## 扩展计划

当前已实现：文本注入、双向文本/图片剪贴板同步、JSON 协议（统一，无旧版兼容）、命令行参数与设备名标识、自动发送、Enter/Tab/Backspace/Copy/Paste 快捷键、鼠标移动/点击/拖动（双击后按住拖动）/滚轮、双向任意文件传输、多网卡 IP 候选筛选与 `--prefer-ip` 收窄、`--token` 固定 token 重启保持 URL。未来计划扩展（详见 `docs/research.md`）：

- 快捷键序列（如 `Cmd+Space`、`Win+R`）
- 安全考虑：快捷键白名单或二次确认机制
