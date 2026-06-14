# qrctrl

## Introduce
A cross-platform tool to use your phone as a remote keyboard for your PC — scan a QR code, type on your phone (or use voice-to-text), and the text is injected into whatever window has focus on the PC. Built with **Rust** + axum + WebSocket.

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

1. Run `qrctrl` on your PC.
2. Scan the QR code displayed in the terminal with your phone's camera or WeChat scanner.
3. The browser opens a textarea — type or use voice input.
4. Press **Enter** (or click **Send**) — text appears in whatever window has focus on the PC.

```
============================================
 qrctrl 已启动
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

- PC runs an HTTP + WebSocket server on port `8080`.
- Phone authenticates via a token embedded in the QR code URL.
- Text sent via WebSocket is injected into the focused window using enigo's `text()` method — Unicode-aware, works with any keyboard layout or input method state.

## Platform Permissions

| Platform | Required permission |
|---|---|
| macOS | System Settings → Privacy & Security → **Accessibility** |
| Windows | None (enigo works without elevated rights) |
| Linux | X11 access (`libxtst`, `libx11`, `libxdo`) |

## Development

```shell
cargo build          # debug build
cargo build --release   # release build (LTO + strip)
cargo test          # run unit tests
```

See [docs/research.md](docs/research.md) for technical research notes on macOS compatibility, enigo capabilities, and future extension plans (shortcut keys, mouse input).

## Roadmap

- [ ] Shortcut keys (e.g. `Cmd+Space`, `Win+R`)
- [ ] Mouse movement and clicks (absolute / relative)
- [ ] Scroll wheel
- [ ] Configurable macro buttons
- [ ] JSON command protocol (backward-compatible with plain text)

## License

MIT
