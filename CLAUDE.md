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
| `--port` | `-p` | `8080`（探测） | 监听端口。不传时从 8080 起按 +1 递增找可用端口（最多到 8129）——双击启动时 8080 被占也能跑起来；显式传时只试这个端口，被占就 panic（尊重用户明确选择） |
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

- **`main.rs`** — 程序入口。用 `clap::Parser`（derive 风格）解析 CLI 参数；`resolve_name()` 决定设备名（`--name` 优先 → 系统主机名 → 兜底 `"qrctrl"`）；`resolve_save_dir()` 解析文件保存目录（main 同步 `create_dir_all`，托盘菜单可能早于 server 线程就绪前被点）；`probe_port(addr, requested)` 同步探测可用端口（见下方「端口策略」）；计算扫码 URL（多网卡筛选 + `--prefer-ip`）；打印 banner（QR 码 + URL）；启动 server 线程（tokio runtime + axum）；主线程跑 `tray::run_tray_event_loop`（阻塞）。tray 退出 → `Notify::notify_waiters` → server graceful shutdown → `join` server 线程后退出。

- **`tray.rs`** — 系统托盘。tao event loop 主线程跑（macOS NSApplication 强制主线程），tray icon + 菜单（复制 URL / 显示二维码 / 打开文件保存目录 / 配置... / 退出）。QR 码窗口用 softbuffer 在 tao Window 上直接画像素，避免依赖系统图片查看器。tray-icon 在 `Event::NewEvents(StartCause::Init)` 内创建（避免过早创建 panic）。`TrayState.save_dir` 用于「打开文件保存目录」菜单项，点击 spawn 子线程跑 `open_in_file_manager`（macOS: `open` / Windows: `explorer` / Linux: `xdg-open`）；「配置...」菜单项把 `state.url` 的 `?t=` 前插 `/config` 构造配置页 URL，spawn 子线程跑 `open_url_in_browser`（macOS: `open` / Windows: `cmd /C start ""` / Linux: `xdg-open`）。`Arc<Notify>` 与 server 线程协调退出。

- **`token.rs`** — 生成一次性随机 token（字母数字组合），用于扫码 URL 鉴权。

- **`config.rs`** — 配置文件 + 配置页 HTTP handlers。三层配置合并（built-in default → `config.toml` → CLI args，CLI 永远赢）。配置文件位置 `dirs::config_dir()/qrctrl/config.toml`，全部字段 `Option<T>`（`None` = 未设置，让下层默认生效）。损坏文件**绝不 panic**——tray app 双击启动下 panic = 静默崩溃，所以 parse 失败时把原文件 rename 成 `config.toml.bad-{timestamp}` 备份后用 default 继续。所有 handler 走 `Query<TokenQuery>` 验证 token（复用 ws.rs 现有鉴权）。`POST /api/config` 通过 axum `Json<Config>` extractor 强制 `Content-Type: application/json`——浏览器跨站 POST 该 Content-Type 会触发 CORS preflight，我们不开 CORS，等于免费 CSRF 防御。`/api/list_dir` 给目录选择模态用（server-driven，浏览器 File System Access API 不暴露绝对路径；只列目录、隐藏 dot 文件降噪）。`/api/local_ips` 给 `--prefer-ip` 下拉框用（复用 `net::list_local_ipv4s`）。`/api/check_port` 给端口字段失焦预检用。**不做 live-apply**：所有改动保存后提示用户重启 qrctrl 让配置生效——部分字段（name / max_size）理论能 live-apply，但混合心智模型反而困惑。

- **`state.rs`** — `AppState` 结构体，包含 `token: String`、`name: String`、`addr: String`、`port: u16`、`prefer_ip: Option<String>`（后三个供配置页 GET 回显用）、`enigo: Arc<Mutex<Enigo>>`、`clipboard: ClipboardHandle`（即 `Arc<Mutex<arboard::Clipboard>>`）、`save_dir: PathBuf`、`max_size: u64`、`registry: TransferRegistry`。`#[derive(Clone)]` 满足 axum State 的 Clone + Send + Sync 要求。enigo 和 clipboard 用**独立 Mutex**——两件事无逻辑互斥关系；共用会拖慢（注入长文本期间无法读剪贴板）。两者都用 `Arc<Mutex<...>>` 包裹，因为 macOS 上 Enigo/Clipboard 都实现了 Send 但没实现 Sync（详见 `tests/send_sync_invariants.rs`）。

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
- **CLI 解析**：`clap = "4"` 的 derive 模式。`Cli` 结构体所有字段都是 `Option<T>`（包括 `addr` / `max_size`），无 `default_value`——main.rs 做**三层合并**：`cli.X.or(file_cfg.X).unwrap_or(BUILTIN_DEFAULT)`，CLI 永远赢，config.toml 是中下层，built-in default 兜底。版本号从 `Cargo.toml` 自动取。`hostname` crate 取系统主机名作为 name 默认值。
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
- **Windows 图标**：与 macOS `.app` 内嵌 `AppIcon.icns` 对应。`build.rs`（项目根，仅 `cfg(target_os = "windows")` 分支生效）调 `winres::WindowsResource` 把 `assets/windows/icon.ico` + FileDescription / ProductName 编译成 .res 资源段嵌入 .exe——资源管理器 / 任务栏 / Alt+Tab 会按场景挑最合适的尺寸渲染。ICO 由 `cargo run --example gen_icon` 从 `assets/icon.png` 用 `image` crate（Lanczos3 缩放）+ `ico` crate（多尺寸封装）生成，固定 7 个尺寸 [16, 24, 32, 48, 64, 128, 256]；源图改了重跑 example 即可，与 macOS 端 `scripts/build-macos-app.sh` 用 `sips`/`iconutil` 一样遵守「`icon.png` 是源真」的约定。`build.rs` 编译失败只 `cargo:warning=...` 不 panic——CI 上偶有缺 rc.exe 的环境仍能产出可运行的二进制，只是没图标；本地装 Windows SDK / Build Tools 后就不会触发。`ico` + `image` 放在 `[dev-dependencies]` 而非 `[build-dependencies]`：example 跑在普通编译环境里，build.rs 跑在 build script 独立环境里，依赖分开。**QR 弹窗图标**（tray.rs `load_window_icon()`）也复用 `assets/icon.png`——通过 `include_bytes!` 编译期内联，再 `image::load_from_memory` → `to_rgba8` → `tao::window::Icon::from_rgba`；`.with_window_icon(Some(...))` 挂到 `WindowBuilder`，Windows 任务栏图标自动跟窗口图标一致。返回 `Option<Icon>` 而非 `expect`，解码失败时窗口仍能创建（默认图标兜底），避免双击启动（无 console）下 silent crash。
- **图标更新流程**：项目里有两个图标源真——`assets/icon.png`（512×512 推荐 1024×1024，主图标）和 `assets/tray-icon.png`（32×32，托盘图标）。所有派生物（Windows `assets/windows/icon.ico`、macOS `AppIcon.icns`、QR 弹窗内嵌图）都从这两个源生成，源图改了要重跑生成步骤 + `cargo build --release` 才生效（源图都是 `include_bytes!` 编译期内联或 build script 加工，运行时不会读盘）。**全套换**：① 覆盖 `assets/icon.png`；② `pwsh scripts/regen-tray-icon.ps1` 自动从 icon.png 缩到 32×32 覆盖 tray-icon.png；③ `cargo run --example gen_icon` 重生成 Windows ICO；④ `cargo build --release`；⑤ macOS 上 `./scripts/build-macos-app.sh`（脚本内部自动 sips+iconutil 重生成 icns）。**只换托盘图标**：替换 `assets/tray-icon.png` + `cargo build --release`。**只换 QR 弹窗 / Windows .exe 图标**：替换 `assets/icon.png`，.exe 还需 `cargo run --example gen_icon`，再 `cargo build --release`。
- **macOS .app bundle**：与 Windows GUI subsystem 对应的 macOS 方案。裸 Mach-O 二进制没 bundle 结构，LaunchServices 当成 CLI 工具，Finder 双击会拉起 Terminal.app 来跑，关 Terminal 就杀进程。`scripts/build-macos-app.sh` 把 `target/release/qrctrl` 组装成 `qrctrl.app/Contents/{MacOS/qrctrl,Info.plist,Resources/AppIcon.icns}`，plist 里设 `LSUIElement=true` 把它标成「背景 UI 应用」(agent / accessory)——Finder 双击不弹 Terminal、不进 Dock、不依附任何父进程，托盘图标与按需弹出的 QR 窗口照常工作。版本号在打包时从 `Cargo.toml` 注入 plist 的 `@VERSION@` 占位符。`CFBundleIconFile=AppIcon` 让 bundle 用 `AppIcon.icns`——由脚本用 macOS 自带的 `sips`（把 `assets/icon.png` 切成 16~1024 共 10 个尺寸）+ `iconutil`（打成 .icns）生成，源图改了重跑脚本即可，与 `assets/tray-icon.png` 同一套「`icon.png` 是源真」的约定。`codesign --sign -` 做 ad-hoc 签名（让 macOS 至少认作合法 bundle；正式发布需开发者证书）。`main()` 的 `has_console` 判断兼容这个路径：`.app` 启动时 stdout 不是 TTY，`is_terminal()` 返回 false → `auto_show_qr = true` → 自动弹 QR 窗口，跟 Windows 双击行为一致。release CI 对每个 macOS target 跑这个脚本，产出 `qrctrl-<arch>-macos.app.zip`（用 `ditto -c -k --keepParent` 压缩，保留可执行权限）和原裸二进制两份 asset。
- **macOS 激活策略**（tray.rs）：光在 plist 设 `LSUIElement=true` 不够——tao 在 `EventLoop` 内部默认 `ActivationPolicy::Regular`，启动时 `launched()` 会调 `NSApp.setActivationPolicy(.regular)` 覆盖 plist 设置，Dock 图标照样冒出来，右键 Dock → Quit 发的 `terminate:` 会杀掉整个托盘进程。修复在 `run_tray_event_loop` 里：构建 `EventLoop` 后立即 `set_activation_policy(Accessory)` + `set_dock_visibility(false)`（这俩改 tao 内部状态，必须在 `run()` 之前调）；`open_qr_window` 里 window `build()` 后再用 `target.set_activation_policy_at_runtime(Accessory)` 强推一次（防御 NSApp 在 window 创建路径里重置策略）。trait 在 `tao::platform::macos`，全部 `#[cfg(target_os = "macos")]` 包，对 Windows/Linux 是 no-op。
- **端口策略**（main.rs）：`--port` 是 `Option<u16>`（无默认值）。`probe_port(addr, requested)` 处理两种语义：`Some(p)` → 只试这个端口，失败 panic（用户显式选择，要尊重）；`None` → 从 8080 起按 +1 递增试 50 个，全失败才 panic。这条主要是为了**双击启动场景**——GUI subsystem / .app bundle 下 stdout 无效，端口冲突时 silent crash 用户根本看不出原因。`probe_port` bind 成功后立即 drop listener，只返回端口号；真正的 listener 在 server 线程内由 `tokio::net::TcpListener::bind` 重新绑定——因为 std listener 跨线程通过 `from_std` 移交给 tokio 后，在 Windows 上 IOCP 注册路径无法正常 accept（端口 LISTENING 但所有连接 timeout，macOS/Linux 无此问题）。TOCTOU 风险窗口是毫秒级，可接受。最终端口写入 URL/banner/tray，QR 码自动指向正确端口。
- **配置系统**（config.rs）：三层合并 `cli.X.or(file_cfg.X).unwrap_or(BUILTIN_DEFAULT)`——CLI 永远赢，`config.toml` 是中下层，built-in default 兜底。配置文件位置 `dirs::config_dir()/qrctrl/config.toml`，全部字段 `Option<T>`（None = 未设置，让下层默认生效）。**损坏文件绝不 panic**——tray app 双击启动下 panic = 静默崩溃，所以 parse 失败时把原文件 rename 成 `config.toml.bad-{timestamp}` 备份后用 default 继续。配置页通过浏览器访问：托盘菜单「配置...」调 `open_url_in_browser(<URL>/config?t=<token>)`，前端 single-file HTML/CSS/JS 在 `static/config.html`（`include_str!` 编译期内联）。所有 `/api/*` 都走 `Query<TokenQuery>` 鉴权。`POST /api/config` 通过 axum `Json<Config>` extractor 强制 `Content-Type: application/json`——跨站 POST 该 Content-Type 触发 CORS preflight，我们不开 CORS，等于免费 CSRF 防御。目录选择走 server-driven modal：浏览器 File System Access API 不暴露绝对路径、rfd 走原生对话框在 macOS 上要主线程跑会卡 tao 事件循环，所以前端拉 `/api/list_dir?path=...` JSON 自己渲染模态。**不做 live-apply**——所有改动保存后提示重启，部分字段（name / max_size）理论能 live-apply，但混合心智模型反而困惑。Token 改变时后端返回 `new_token`，前端更新内存中的 currentToken 用于后续 fetch（旧 URL 立即失效）；托盘菜单的「复制 URL」/「显示二维码」要等重启才指向新 token。
- **错误处理**：启动期错误（端口绑定失败、enigo/clipboard 初始化失败）用 `expect` 直接 panic；运行时错误（WebSocket 错误、注入失败、剪贴板错误）用 `eprintln!` 记录后继续，不退出服务器。`CbError` 映射为协议错误码字符串（`empty` / `clipboard_busy` / `decode_failed` / `too_large` / `internal`）。

## 扩展计划

当前已实现：文本注入、双向文本/图片剪贴板同步、JSON 协议（统一，无旧版兼容）、命令行参数与设备名标识、自动发送、Enter/Tab/Backspace/Copy/Paste 快捷键、鼠标移动/点击/拖动（双击后按住拖动）/滚轮、双向任意文件传输、多网卡 IP 候选筛选与 `--prefer-ip` 收窄、`--token` 固定 token 重启保持 URL、系统托盘 + 后台运行（Windows GUI subsystem / macOS `.app` + `LSUIElement`）、双击自动弹 QR、端口冲突时自动递增（双击启动不崩溃）、托盘菜单打开文件保存目录、浏览器配置页（托盘菜单 → 配置... → 所有参数可编辑 → 保存到 config.toml → 重启生效）。未来计划扩展（详见 `docs/future.md`）：

- 快捷键序列（如 `Cmd+Space`、`Win+R`）
- 安全考虑：快捷键白名单或二次确认机制
