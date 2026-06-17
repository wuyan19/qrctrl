# qrctrl

[English](README.md) | **[中文](README_zh.md)**

## 介绍

把手机变成 PC 遥控器的跨平台工具——扫一个二维码，就能：

- **手机打字 → PC 焦点窗口**（键盘 / 语音转写 / emoji）
- **双向剪贴板** — 拉取 PC 文本/图片到手机，推送手机图片到 PC
- **双向文件传输** — 手机文件上传到 PC 保存目录，PC 文件下载到手机
- **鼠标控制** — 触控板表面支持移动 / 点击 / 双击后按住拖动 / 滚轮
- **快捷键** — Enter / Tab / Backspace / Copy / Paste
- **自动发送** — 输入停顿后自动发送（IME 安全，中文拼音选词期间不触发）
- **系统托盘 + 后台运行** — 关终端不会杀进程；Windows 下双击 exe、macOS 下双击 .app 直接后台运行
- **Token 持久化** — `--token` 让扫码 URL 在重启后保持不变
- **多设备** — 给每台 PC 设 `--name`，手机端可区分

用 **Rust** + axum + WebSocket 构建。跨平台（macOS / Windows / Linux），手机端无需安装 App，浏览器即可。

## 安装

### 从源码编译

```shell
cd qrctrl
cargo install --path .
```

### 从 Release 下载

在 [Releases](https://github.com/wuyan19/qrctrl/releases) 页面下载对应平台的二进制。

> **macOS 用户**：每个架构发布两个产物——裸二进制（`qrctrl-<arch>-macos`）和 `.app` 包（`qrctrl-<arch>-macos.app.zip`）。
>
> - **要双击后台运行**：下 `.app.zip`，Finder 双击解压，再双击 `qrctrl.app`。不弹 Terminal、不进 Dock、只有托盘。拖进 `/Applications` 后可从 Spotlight 启动。
> - **要走命令行**：下裸二进制，`chmod +x`，在 shell 里运行。
>
> bundle 未签名（仅 ad-hoc 签名）。首次启动会被 macOS 拦截「来自身份不明的开发者」：
> 1. 解压后在 Finder 右键 `qrctrl.app` → **打开** → 弹窗里再点 **打开**。一次性放行，后续直接双击即可。
> 2. 或在终端运行 `xattr -dr com.apple.quarantine /path/to/qrctrl.app`
> 3. 在「系统设置 → 隐私与安全性 → 辅助功能」中授予 **Accessibility** 权限（键盘注入必需）。

> **Windows 用户**：release 构建使用 GUI 子系统——在资源管理器双击 `qrctrl.exe` 即可后台运行（无 cmd 黑窗，无父终端可关）。首次启动会自动弹出二维码窗口。系统托盘图标提供菜单（复制 URL / 显示二维码 / 退出）。

## 使用

```shell
qrctrl                                  # 默认 0.0.0.0:8080，name = 主机名
qrctrl --port 9000                      # 自定义端口
qrctrl --name "工作 Mac"                 # 自定义设备名
qrctrl --token mytoken123               # 固定 token：重启后 URL 不变
qrctrl --prefer-ip 192.168.20           # 多网卡：指定 QR 码用哪个子网的 IP
qrctrl --save-dir ~/Downloads/qrctrl    # 手机上传文件的保存目录
qrctrl -a 127.0.0.1 -p 9000 -n "测试"   # 短参数形式
```

| 参数 | 短 | 默认 | 说明 |
|---|---|---|---|
| `--addr` | `-a` | `0.0.0.0` | 监听地址 |
| `--port` | `-p` | `8080`（探测） | 监听端口。不传时从 8080 起按 +1 递增找可用端口（最多到 8129）——双击启动时 8080 被占也能自动跑起来；显式传时只试这个端口，被占就退出 |
| `--name` | `-n` | 系统主机名 | 设备名（手机端状态栏显示） |
| `--save-dir` | — | `<下载目录>/qrctrl/` | 手机上传文件的保存目录 |
| `--max-size` | — | `10737418240`（10 GB） | 单文件大小上限（字节） |
| `--token` | — | 每次启动随机生成 | 固定 token（4-64 位 ASCII 字母数字）。提供后重启扫码 URL 不变，手机端刷新页面即可重连 |
| `--prefer-ip` | — | — | 偏好的 IP 子网前缀（如 `192.168.20`），多网卡时用来挑 QR 码用的 IP。不匹配时回退到全部候选 |
| `--help` | `-h` | — | 帮助 |
| `--version` | `-V` | — | 版本（来自 Cargo.toml） |

1. 在 PC 上运行 `qrctrl`。
2. 用手机相机或微信扫一扫扫描二维码（终端 banner 或自动弹出的二维码窗口）。
3. 浏览器打开控制面板，包含文本输入、剪贴板按钮、文件传输、触控板。

### 文本输入（手机 → PC）

打字或语音输入，按 **Enter**（或点击 **发送**）。文本注入到 PC 当前焦点窗口。

开启 **⚡ 自动发送** 复选框后，输入停顿 600ms 自动发送——IME 安全，拼音选词期间不触发。

### 剪贴板同步（双向）

**PC → 手机：**
- **📋 拉文本** — 读取 PC 剪贴板文本。写入手机剪贴板（HTTP 下 `navigator.clipboard` 不可用时回退到 textarea）。
- **🖼 拉图片** — 读取 PC 剪贴板图片。弹出预览模态——iOS / Android 长按可保存/复制。

**手机 → PC：**
- **📷 选图上传** — 从手机相册/相机选图。PC 写入剪贴板，然后 `Cmd+V` / `Ctrl+V` 粘贴到任意 App。
- **粘贴截图** — 手机截图（电源+音量上等），然后长按 textarea → 粘贴。与选图走同一条剪贴板写入路径。

> **macOS 提示**：`Cmd+Shift+4` 截图是**存文件**，不进剪贴板。要直接进剪贴板用 `Ctrl+Cmd+Shift+4`。如果从 Finder 对截图文件 `Cmd+C`，qrctrl 也能识别——会通过 `arboard::Clipboard::file_list()` 读实际文件，而不是系统生成的占位图标。

### 文件传输（双向）

- **手机 → PC 上传**：从手机选任意文件。通过 HTTP 流式上传（`POST /upload/{id}`）到 `--save-dir`（默认 `<下载目录>/qrctrl/`）。单文件大小受 `--max-size` 限制。
- **PC → 手机 下载**：在面板长按文件链接，从 PC 保存目录下载。

文件注册到传输注册表并带过期；后台任务每 60s 清理过期条目。

### 鼠标控制（手机 → PC）

手机面板有 **触控板** 表面：
- **移动** — 拖动移动光标（相对位移）。
- **点击** — 轻点左键。提供左/右/中键按钮。
- **拖动** — 双击后按住：300ms 内第一次轻点，然后再按下并拖动（macOS 风格 tap-to-drag）。
- **滚轮** — 在滚动区域垂直拖动，速度可调。

### Token 持久化

默认每次启动生成随机 token——QR 码每次都变。传 `--token <固定值>`（4-64 位 ASCII 字母数字）后 URL 跨重启保持不变；手机端刷新页面即可重连。

### 多设备

每台 PC 跑 `qrctrl --name "<标签>"`。手机状态栏显示 `<name> 已连接` / `<name> 已断开`，提示也含名称（如 `已写入 <name> 剪贴板`），始终清楚在控制哪台机器。

### 浏览器配置页

双击启动的用户没有终端——但他们还是需要改参数。打开托盘菜单 → **配置...**，系统浏览器打开 `<URL>/config?t=<token>`，所有参数都在一个表单里：

- **name / addr** — 文本输入框。
- **port** — 数字输入框，失焦时预检端口可用性（再也不会重启后才发现 8080 被占导致 silent crash）。
- **save_dir** — 只读输入框 + **浏览...** 按钮，弹出 server-driven 目录选择模态（面包屑导航 + 点击逐层下降）。
- **max_size** — 数字 + KB/MB/GB 单位选择器。
- **prefer_ip** — 下拉框，选项来自 `/api/local_ips`，含「无偏好」。
- **token** — 文本输入框，带显示/隐藏切换。修改后旧扫码 URL 立即失效——页面会显示新 URL 让你重新扫。

保存 → 写入 `config.toml` → 黄色横幅：「已保存。**请重启 qrctrl 让配置生效。**」所有改动重启后生效——qrctrl 不做局部 live-apply。

配置文件位置 `dirs::config_dir()/qrctrl/config.toml`：

- Windows: `%APPDATA%\qrctrl\config.toml`
- macOS: `~/Library/Application Support/qrctrl/config.toml`
- Linux: `~/.config/qrctrl/config.toml`

Schema（全部字段可选——没写 = 让下层默认生效）：

```toml
addr = "0.0.0.0"
port = 8080
name = "工作 Mac"
save_dir = "/Users/wuyan/Downloads/qrctrl"
max_size = 10737418240
token = "abc123def456"
prefer_ip = "192.168.20"
```

配置三层合并：**CLI 参数 > config.toml > 内置默认**。CLI 永远赢；不传则用 config.toml；config.toml 也没写就用内置默认。损坏的 config.toml 永远不会让程序崩——qrctrl 会把它改名成 `config.toml.bad-{timestamp}` 备份后用 default 继续（tray app 在配置 typo 上静默崩溃是最糟糕的体验）。

### 系统托盘 + 后台运行

运行时 qrctrl 常驻系统托盘，菜单：
- **复制 URL** — 把扫码 URL 写入剪贴板，便于手动分享。
- **显示二维码** — 关掉二维码窗口后重新弹出。
- **打开文件保存目录** — 在系统文件管理器里打开手机上传文件的保存目录（macOS Finder / Windows 资源管理器 / Linux xdg-open）。
- **配置...** — 打开浏览器配置页，让双击启动（无终端）的用户也能改参数。
- **退出** — 触发 graceful shutdown（上传中的文件先完成再退出）。

Windows 下 release 构建是 GUI 子系统二进制——从资源管理器双击 `qrctrl.exe` 静默启动（无 cmd 黑窗，无父终端可误关）。首次启动自动弹出二维码窗口。从终端（PowerShell / cmd）启动时 banner 仍正常打印。

端口冲突时自动递增：不传 `--port` 时从 8080 起试，被占就 +1 直到找到可用（最多到 8129）；显式传 `--port` 时只试这个端口，被占直接退出（尊重用户明确选择）。这样双击启动遇到 8080 被占（其他开发服务、代理等常见）也不会静默崩溃。

macOS 下 `.app` bundle 的 `Info.plist` 设了 `LSUIElement=true`，Finder 双击 `qrctrl.app` 会以「背景 UI 应用」(agent) 方式启动——不弹 Terminal、不进 Dock，关任何窗口（或注销重登）都不会杀进程。首次启动自动弹出二维码窗口（此路径下 stdout 不是 TTY，触发自动弹窗逻辑）。命令行用户仍可走 `qrctrl.app/Contents/MacOS/qrctrl` 在 shell 里跑，banner 照常打印。

## 工作原理

- PC 在配置的 `addr:port` 上跑 HTTP + WebSocket 服务。HTTP 层在 `/` 提供静态控制面板，在 `/upload/{id}` + `/download/{id}` 流式传输文件。
- 手机通过二维码 URL 中嵌入的 token 鉴权。
- WebSocket 升级后，服务端立刻推送 `{"type":"server_info","name":"..."}`，前端用于状态栏/提示文字。
- 其他 WebSocket 消息都是 JSON，带 `type` 字段（`text` / `get_clipboard_text` / `get_clipboard_image` / `set_clipboard_image` / `upload_start` / `get_file` / `enter` / `tab` / `backspace` / `copy` / `paste` / `mouse_move` / `mouse_click` / `mouse_press` / `mouse_release` / `mouse_scroll`）。
- 文本注入走 enigo 的 `text()` 方法——Unicode 路径，不依赖键盘布局或输入法状态。
- 剪贴板访问走 arboard（文本 + 图片 + 文件列表）。读路径上文件引用会读实际文件——用户 `Cmd+C` 文件而不是复制图片内容时这点很关键。
- 鼠标事件走 enigo（macOS CGEvent、Windows SendInput、Linux XTest）。
- 阻塞调用（enigo、arboard）都丢到 `tokio::task::spawn_blocking`，避免阻塞异步 runtime。
- Server 线程跑 tokio runtime；主线程跑 tao 事件循环驱动托盘（macOS 要求 NSApplication 在主线程）。退出通过 `tokio::sync::Notify` 协调。

## 平台权限

| 平台 | 必需权限 |
|---|---|
| macOS | 系统设置 → 隐私与安全性 → **辅助功能**（键盘 + 鼠标注入）；监听端口入站的防火墙弹窗。每次重新编译后 TCC 指纹变化，可能需要重新授权。 |
| Windows | 无需特殊权限（enigo 不需要管理员权限）。Release 构建为 GUI 子系统，无父终端。 |
| Linux | X11 访问（`libxtst`、`libx11`、`libxdo`）；托盘需要 GTK + appindicator（`libgtk-3-dev`、`libayatana-appindicator3-dev`）。 |

## 开发

```shell
cargo build              # 调试构建（Windows 下 console 子系统——println! 可见）
cargo build --release    # 发布构建（Windows 下 GUI 子系统，LTO + strip）
cargo test               # 跑单元测试
```

本地组装 macOS `.app` bundle：

```shell
cargo build --release
./scripts/build-macos-app.sh                            # 产出 target/release/qrctrl.app
TARGET_TRIPLE=aarch64-apple-darwin ./scripts/build-macos-app.sh  # 跨 target
```

未实现功能的设计草稿（快捷键序列、Metadata+Blob 架构、TLS、宏按钮）见 [docs/future.md](docs/future.md)。

从新的 `assets/icon.png` 重新生成 `assets/tray-icon.png`：

```shell
powershell -ExecutionPolicy Bypass -File scripts\regen-tray-icon.ps1
```

## 路线图

已发布：

- [x] JSON 指令协议（统一，无纯文本回退）
- [x] 双向文本 + 图片剪贴板同步
- [x] 双向任意文件传输
- [x] 鼠标移动 / 点击 / 双击后按住拖动 / 滚轮
- [x] 快捷键（Enter / Tab / Backspace / Copy / Paste）
- [x] 命令行配置（`--addr` / `--port` / `--name` / `--save-dir` / `--max-size`）
- [x] 多网卡 IP 选择（`--prefer-ip`）
- [x] 固定 token 保持扫码 URL（`--token`）
- [x] 系统托盘 + graceful shutdown
- [x] Windows（GUI 子系统）与 macOS（`.app` bundle + `LSUIElement`）后台运行
- [x] 双击启动自动弹出二维码窗口
- [x] 浏览器配置页（托盘 → 配置... → 所有参数可编辑 → 写入 config.toml → 重启生效）

未来计划：

- [ ] 快捷键序列（如 `Cmd+Space`、`Win+R`）——可能配白名单或二次确认机制
- [ ] 可配置的宏按钮 / 面板
- [ ] TLS / HTTPS 支持（当前是明文 HTTP——局域网可用，但在不受信任的网络不安全）

## 协议

MIT
