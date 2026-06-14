# qrctrl

## Introduce
A cross-platform tool that turns your phone into a remote control for your PC — scan a QR code, then:

- **Type on phone → PC focus window** (keyboard / voice-to-text / emoji)
- **Pull PC clipboard to phone** (text → phone clipboard; image → preview modal)
- **Push phone image to PC clipboard** (file picker or paste a screenshot)
- **Auto-send mode** — text transmits automatically after typing pauses (60 ms IME-safe)
- **Multiple devices** — give each PC a `--name` so the phone UI can tell them apart

Built with **Rust** + axum + WebSocket. Cross-platform (macOS / Windows / Linux), no client app install — just a browser.

## Install

### From Source

```shell
cd qrctrl
cargo install --path .
```

### From Release

Download the binary for your platform from the [Releases](https://github.com/wuyan19/qrctrl/releases) page.

> **macOS Users**: The binary is unsigned. After downloading:
> 1. If macOS blocks it with "cannot be opened because it is from an unidentified developer", run:
>    ```shell
>    xattr -d com.apple.quarantine qrctrl-*
>    ```
> 2. Grant **Accessibility** permission in System Settings → Privacy & Security → Accessibility (required for keyboard input).

## Usage

```shell
qrctrl                                  # default 0.0.0.0:8080, name = hostname
qrctrl --port 9000                      # custom port
qrctrl --name "工作 Mac"                 # custom device name
qrctrl -a 127.0.0.1 -p 9000 -n "Test"   # short flags
```

| Option | Short | Default | Description |
|---|---|---|---|
| `--addr` | `-a` | `0.0.0.0` | Listen address |
| `--port` | `-p` | `8080` | Listen port |
| `--name` | `-n` | system hostname | Device name shown on the phone UI |
| `--help` | `-h` | — | Print help |
| `--version` | `-V` | — | Print version |

1. Run `qrctrl` on your PC.
2. Scan the QR code displayed in the terminal with your phone's camera or WeChat scanner.
3. The browser opens a control panel with text input + tool buttons.

### Text input (phone → PC)

Type or use voice input, press **Enter** (or click **Send**). Text appears in whatever window has focus on the PC.

Enable the **⚡ 自动发送** checkbox to transmit automatically after typing pauses (600 ms) — IME-safe, won't fire mid-pinyin.

### Pull PC clipboard (PC → phone)

- **📋 拉文本** — Reads PC clipboard text. Writes to phone clipboard (or textarea fallback if `navigator.clipboard` is unavailable on HTTP).
- **🖼 拉图片** — Reads PC clipboard image. Pops a preview modal — long-press to save / copy on iOS or Android.

> **macOS tip**: `Cmd+Shift+4` saves a screenshot to the **file**, not the clipboard. Use `Ctrl+Cmd+Shift+4` to capture directly to clipboard. If you `Cmd+C` a screenshot file from Finder, that works too — qrctrl reads the actual file via `arboard::Clipboard::file_list()` rather than the OS-generated placeholder icon.

### Push phone image (phone → PC)

- **📷 选图上传** — Pick an image from phone album / camera. PC writes it to clipboard, then `Cmd+V` / `Ctrl+V` to paste into any app.
- **Paste screenshot** — Screenshot on phone (Power+VolumeUp etc.), then long-press the textarea → Paste. Same clipboard-write path as file picker.

### Multiple devices

Run `qrctrl --name "<label>"` on each PC. The phone status bar shows `<name> 已连接` / `<name> 已断开` and toasts include the name (e.g. `已写入 <name> 剪贴板`), so you always know which machine you're controlling.

```
============================================
 qrctrl 已启动 · 设备名：工作 Mac
--------------------------------------------
 手机扫码连接（相机/微信扫一扫）：

 ▄▄▄▄▄▄▄ ▄▄▄▄▄ ▄▄▄▄▄ ▄▄▄▄▄ ▄▄▄▄▄▄▄
 ...

--------------------------------------------
 或手动输入 URL：
   http://192.168.1.100:8080/?t=abc123
============================================
```

## How It Works

- PC runs an HTTP + WebSocket server on the configured `addr:port`.
- Phone authenticates via a token embedded in the QR code URL.
- Server pushes `{"type":"server_info","name":"..."}` immediately after WebSocket upgrade — the phone uses this for status / toast text.
- All other WebSocket messages are JSON with a `type` field (`text` / `get_clipboard_text` / `get_clipboard_image` / `set_clipboard_image`).
- Text injection uses enigo's `text()` method — Unicode-aware, works with any keyboard layout or input method state.
- Clipboard access via arboard (text + image + file list). On the read path, file references are resolved by reading the underlying file — this matters when the user `Cmd+C`'s a file rather than copying image content directly.
- Blocking calls (enigo, arboard) run on `tokio::task::spawn_blocking` to avoid stalling the async runtime.

## Platform Permissions

| Platform | Required permission |
|---|---|
| macOS | System Settings → Privacy & Security → **Accessibility** (for keyboard injection); firewall prompt for inbound on the listen port |
| Windows | None (enigo works without elevated rights) |
| Linux | X11 access (`libxtst`, `libx11`, `libxdo`) |

## Development

```shell
cargo build          # debug build
cargo build --release   # release build (LTO + strip)
cargo test          # run unit tests
```

See [docs/research.md](docs/research.md) for technical research notes on macOS compatibility, enigo capabilities, the bidirectional clipboard design, and future extension plans (shortcut keys, mouse input, arbitrary file transfer).

## Roadmap

Shipped:

- [x] JSON command protocol (unified, no plain-text fallback)
- [x] Bidirectional text + image clipboard sync
- [x] CLI configuration (`--addr` / `--port` / `--name`)
- [x] Device name display on phone UI
- [x] Auto-send mode (IME-safe)

Planned for upcoming versions:

- [ ] Shortcut keys (e.g. `Cmd+Space`, `Win+R`)
- [ ] Mouse movement and clicks (absolute / relative)
- [ ] Scroll wheel
- [ ] Configurable macro buttons
- [ ] Arbitrary file transfer (unified channel for text / image / file — needs HTTP streaming endpoint for large files)

## License

MIT
