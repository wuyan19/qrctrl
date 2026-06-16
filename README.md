# qrctrl

**[English](README.md)** | [中文](README_zh.md)

## Introduce

A cross-platform tool that turns your phone into a remote control for your PC — scan a QR code, then:

- **Type on phone → PC focus window** (keyboard / voice-to-text / emoji)
- **Bidirectional clipboard** — pull PC text/image to phone, push phone image to PC
- **Bidirectional file transfer** — push phone files to PC's save dir, pull PC files down to phone
- **Mouse control** — move / click / tap-tap-drag / scroll from a touchpad surface
- **Shortcut keys** — Enter / Tab / Backspace / Copy / Paste
- **Auto-send mode** — text transmits automatically after typing pauses (IME-safe)
- **System tray + background run** — closes terminal won't kill the program; on Windows double-click the exe to run silently in the background
- **Token persistence** — `--token` keeps the scan URL stable across restarts
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

> **Windows Users**: The release build uses the GUI subsystem — double-click `qrctrl.exe` in File Explorer and it runs silently in the background (no cmd window, no terminal parent to kill). The QR window auto-opens on first launch. The system tray icon provides a menu (Copy URL / Show QR / Quit).

## Usage

```shell
qrctrl                                  # default 0.0.0.0:8080, name = hostname
qrctrl --port 9000                      # custom port
qrctrl --name "Work Mac"                # custom device name
qrctrl --token mytoken123               # fixed token: URL stays same across restarts
qrctrl --prefer-ip 192.168.20           # multi-NIC: pick which subnet shows up in the QR
qrctrl --save-dir ~/Downloads/qrctrl    # where phone-uploaded files land
qrctrl -a 127.0.0.1 -p 9000 -n "Test"   # short flags
```

| Option | Short | Default | Description |
|---|---|---|---|
| `--addr` | `-a` | `0.0.0.0` | Listen address |
| `--port` | `-p` | `8080` | Listen port |
| `--name` | `-n` | system hostname | Device name shown on the phone UI |
| `--save-dir` | — | `<download>/qrctrl/` | Where phone-uploaded files are saved |
| `--max-size` | — | `10737418240` (10 GB) | Per-file size limit in bytes |
| `--token` | — | random per launch | Fixed token (4–64 ASCII alphanumeric). Keeps scan URL stable across restarts — phone just refreshes the page to reconnect |
| `--prefer-ip` | — | — | Preferred subnet prefix (e.g. `192.168.20`) for picking the QR IP when multiple NICs are present. Falls back to all candidates if no match |
| `--help` | `-h` | — | Print help |
| `--version` | `-V` | — | Print version |

1. Run `qrctrl` on your PC.
2. Scan the QR code (terminal banner or the auto-opened QR window) with your phone's camera or WeChat scanner.
3. The browser opens a control panel with text input, clipboard buttons, file transfer, and a touchpad.

### Text input (phone → PC)

Type or use voice input, press **Enter** (or click **Send**). Text appears in whatever window has focus on the PC.

Enable the **⚡ auto-send** checkbox to transmit automatically after typing pauses (600 ms) — IME-safe, won't fire mid-pinyin.

### Clipboard sync (bidirectional)

**PC → phone:**
- **📋 Pull text** — Reads PC clipboard text. Writes to phone clipboard (or textarea fallback if `navigator.clipboard` is unavailable on HTTP).
- **🖼 Pull image** — Reads PC clipboard image. Pops a preview modal — long-press to save / copy on iOS or Android.

**Phone → PC:**
- **📷 Pick image** — Pick an image from phone album / camera. PC writes it to clipboard, then `Cmd+V` / `Ctrl+V` to paste into any app.
- **Paste screenshot** — Screenshot on phone (Power+VolumeUp etc.), then long-press the textarea → Paste. Same clipboard-write path as file picker.

> **macOS tip**: `Cmd+Shift+4` saves a screenshot to the **file**, not the clipboard. Use `Ctrl+Cmd+Shift+4` to capture directly to clipboard. If you `Cmd+C` a screenshot file from Finder, that works too — qrctrl reads the underlying file via `arboard::Clipboard::file_list()` rather than the OS-generated placeholder icon.

### File transfer (bidirectional)

- **Upload phone → PC**: Pick any file from the phone. It's streamed to the PC over HTTP (`POST /upload/{id}`) into `--save-dir` (default `<download>/qrctrl/`). Single-file size capped by `--max-size`.
- **Download PC → phone**: Long-press a file link in the panel to download from the PC's save dir.

Files are registered in a transfer registry with expiration; a background task cleans up stale entries every 60 s.

### Mouse control (phone → PC)

The phone panel has a **touchpad** surface:
- **Move** — drag to move the cursor (relative delta).
- **Click** — tap to left-click. Buttons provided for left/right/middle click.
- **Drag** — tap-tap-hold: first tap within 300 ms, then press and drag (macOS-style tap-to-drag).
- **Scroll** — vertical drag on the scroll area, with adjustable speed.

### Token persistence

By default, qrctrl generates a random token each launch — the QR code changes every time. Pass `--token <fixed>` (4–64 ASCII alphanumeric) and the URL stays stable across restarts; the phone just refreshes the page to reconnect.

### Multiple devices

Run `qrctrl --name "<label>"` on each PC. The phone status bar shows `<name> connected` / `<name> disconnected` and toasts include the name (e.g. `written to <name> clipboard`), so you always know which machine you're controlling.

### System tray + background run

Once running, qrctrl lives in the system tray with a menu:
- **Copy URL** — puts the scan URL on the clipboard for manual sharing.
- **Show QR** — reopens the QR window if you closed it.
- **Quit** — triggers graceful shutdown (in-flight uploads finish before exit).

On Windows the release build is a GUI-subsystem binary — double-clicking `qrctrl.exe` from File Explorer launches it silently (no cmd window, no parent terminal to accidentally close). The QR window auto-opens on first launch. From a terminal (PowerShell / cmd) the banner is still printed normally.

## How It Works

- PC runs an HTTP + WebSocket server on the configured `addr:port`. The HTTP layer serves the static control panel at `/` and streams files at `/upload/{id}` + `/download/{id}`.
- Phone authenticates via a token embedded in the QR code URL.
- Server pushes `{"type":"server_info","name":"..."}` immediately after WebSocket upgrade — the phone uses this for status / toast text.
- All other WebSocket messages are JSON with a `type` field (`text` / `get_clipboard_text` / `get_clipboard_image` / `set_clipboard_image` / `upload_start` / `get_file` / `enter` / `tab` / `backspace` / `copy` / `paste` / `mouse_move` / `mouse_click` / `mouse_press` / `mouse_release` / `mouse_scroll`).
- Text injection uses enigo's `text()` method — Unicode-aware, works with any keyboard layout or input method state.
- Clipboard access via arboard (text + image + file list). On the read path, file references are resolved by reading the underlying file — this matters when the user `Cmd+C`'s a file rather than copying image content directly.
- Mouse events go through enigo (CGEvent on macOS, SendInput on Windows, XTest on Linux).
- Blocking calls (enigo, arboard) run on `tokio::task::spawn_blocking` to avoid stalling the async runtime.
- Server thread runs the tokio runtime; main thread runs the tao event loop for the tray (macOS requires NSApplication on the main thread). Quit is coordinated via `tokio::sync::Notify`.

## Platform Permissions

| Platform | Required permission |
|---|---|
| macOS | System Settings → Privacy & Security → **Accessibility** (for keyboard + mouse injection); firewall prompt for inbound on the listen port. After recompiling, the TCC fingerprint changes — may need re-granting. |
| Windows | None (enigo works without elevated rights). Release build runs as GUI subsystem, no terminal parent. |
| Linux | X11 access (`libxtst`, `libx11`, `libxdo`); tray needs GTK + appindicator (`libgtk-3-dev`, `libayatana-appindicator3-dev`). |

## Development

```shell
cargo build              # debug build (console subsystem on Windows — println! visible)
cargo build --release    # release build (GUI subsystem on Windows, LTO + strip)
cargo test               # run unit tests
```

See [docs/future.md](docs/future.md) for design notes on unimplemented features (shortcut sequences, metadata+blob architecture, TLS, macro buttons).

To regenerate `assets/tray-icon.png` from a new `assets/icon.png`:

```shell
powershell -ExecutionPolicy Bypass -File scripts\regen-tray-icon.ps1
```

## Roadmap

Shipped:

- [x] JSON command protocol (unified, no plain-text fallback)
- [x] Bidirectional text + image clipboard sync
- [x] Bidirectional arbitrary file transfer
- [x] Mouse movement / click / tap-tap-drag / scroll
- [x] Shortcut keys (Enter / Tab / Backspace / Copy / Paste)
- [x] CLI configuration (`--addr` / `--port` / `--name` / `--save-dir` / `--max-size`)
- [x] Multi-NIC IP selection (`--prefer-ip`)
- [x] Fixed token for stable scan URL (`--token`)
- [x] System tray + graceful shutdown
- [x] Background run on Windows (GUI subsystem)
- [x] Auto-open QR window on double-click launch

Planned for upcoming versions:

- [ ] Shortcut-key sequences (e.g. `Cmd+Space`, `Win+R`) — likely with a whitelist or confirmation mechanism
- [ ] Configurable macro buttons / panels
- [ ] TLS / HTTPS support (currently plaintext HTTP — fine for LAN, but unsafe on untrusted networks)

## License

MIT
