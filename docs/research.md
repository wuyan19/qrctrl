# qrctrl 调研记录

本文档汇总项目当前已完成的调研内容，供后续扩展功能（快捷键、鼠标等）时参考。

---

## 一、macOS 兼容性调研

### 1.1 代码层面

- **`enigo = "0.6"` 原生跨平台**：Windows / macOS / Linux 内置分支，无需 features 切换。
- 其他依赖（axum、tokio、qrcode、local-ip-address、rand）均跨平台。
- 已扫描全部源码：没有任何 Windows 专用 API、`#[cfg(windows)]` 之类的硬编码。
- `tests/send_sync_invariants.rs:23` 的注释已经识别到「macOS 上 Enigo 只有 Send 没有 Sync」，并用 `Arc<Mutex<Enigo>>` 正确处理了（这是 macOS 兼容性最常踩的坑，已经规避）。

### 1.2 运行层面必须授权的权限

| 权限 | 触发时机 | 没授权的表现 |
|---|---|---|
| **辅助功能（Accessibility）** | 首次 `enigo.text()` 调用 | 系统设置 → 隐私与安全性 → 辅助功能 → 勾上终端/二进制。系统弹窗**只弹一次**，错过要手动加 |
| **防火墙入站** | `TcpListener::bind("0.0.0.0:8080")` 启动时 | 弹窗拒绝则手机连不上局域网服务，只能 localhost 自测 |

### 1.3 编译方式

```bash
# 在 macOS 上原生编译，一条命令搞定
cargo build --release

# Apple Silicon + Intel 都支持
# 想做 Universal Binary：
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
lipo -create -output qrctrl \
  target/aarch64-apple-darwin/release/qrctrl \
  target/x86_64-apple-darwin/release/qrctrl
```

从 Windows/Linux 交叉编译到 macOS 不现实（需要 macOS SDK + osxcross，许可证问题），**只能在 macOS 上原生构建**。

### 1.4 实测需要重点验证的点

1. **中文输入法状态**：enigo 在 macOS 上用 `CGEventCreateUnicodeString` 注入完整 Unicode 字符（不模拟按键），所以理论上**不依赖当前输入法状态**，中文/英文输入法都应直接出字。但要实测验证。
2. **重新编译后的权限失效**：macOS TCC 会按签名/可执行文件指纹记录权限，**每次重新 `cargo build` 后可能需要在系统设置里把旧条目删掉重新授权**（这是 macOS 自动化工具的通病，不是 enigo 的锅）。
3. **emoji 输入**：lib.rs 文档明确支持 emoji（`"Hello ❤️"`），但需要实测。

---

## 二、enigo 0.6 完整能力调研

enigo 0.6 是为远程控制场景设计的（官方文档明确说「可用于构建远程控制应用」）。除了当前用到的文本注入，还支持按键、鼠标、滚动。

### 2.1 完整 API

```rust
use enigo::{
    Button, Coordinate,
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Mouse, Settings, Axis,
};

let mut enigo = Enigo::new(&Settings::default()).unwrap();

// 1. 文本注入（当前已用）
enigo.text("hello world ❤️");

// 2. 快捷键（Press / Release / Click 三种时序）
enigo.key(Key::Control, Press);
enigo.key(Key::Unicode('v'), Click);
enigo.key(Key::Control, Release);

// 3. 鼠标移动（绝对 / 相对）
enigo.move_mouse(500, 200, Coordinate::Abs);
enigo.move_mouse(100, 100, Coordinate::Rel);

// 4. 鼠标点击
enigo.button(Button::Left, Press);
enigo.button(Button::Left, Release);

// 5. 滚轮
enigo.scroll(1, Axis::Horizontal);
enigo.scroll(-3, Axis::Vertical);
```

### 2.2 三种按键 API 的区别

| 方法 | 用途 | 适用场景 |
|---|---|---|
| `text(&str)` | 输入文本（Unicode 字符） | 跨键盘布局输入文本、emoji |
| `key(Key, Direction)` | 按虚拟键（keysym） | 快捷键、功能键，系统会做键盘布局转换 |
| `raw(u16, Direction)` | 按物理 keycode（scancode） | 游戏 WASD，无关布局 |

### 2.3 打开 APP 的常见快捷键

- macOS：`Cmd+Space`（Spotlight）、`Cmd+Tab`（切换）、`Cmd+Option+Esc`（强制退出）
- Windows：`Win+R`（运行）、`Win+S`（搜索）、`Win+1..9`（任务栏第 N 个）
- Linux：`Super`（活动概览）

---

## 三、协议扩展设计

当前前端发送纯文本字符串，扩展后改成 JSON 指令。

### 3.1 指令格式

```json
{"type":"text","value":"你好"}
{"type":"keys","sequence":["Cmd","Space"]}
{"type":"move","x":500,"y":300,"mode":"abs"}
{"type":"click","button":"left","x":500,"y":300}
{"type":"scroll","delta":-3,"axis":"vertical"}
```

### 3.2 向后兼容策略

纯字符串消息（不以 `{` 开头）仍按 `text` 处理，保持现有前端向后兼容。

### 3.3 后端分发逻辑（伪代码）

```rust
match msg {
    Message::Text(s) if !s.starts_with('{') => inject_text(enigo, &s),
    Message::Text(json) => {
        let cmd: Command = serde_json::from_str(&json)?;
        match cmd {
            Command::Text { value } => inject_text(enigo, &value),
            Command::Keys { sequence } => inject_keys(enigo, &sequence),
            Command::Move { x, y, mode } => inject_move(enigo, x, y, mode),
            Command::Click { button, x, y } => inject_click(enigo, button, x, y),
            Command::Scroll { delta, axis } => inject_scroll(enigo, delta, axis),
        }
    }
    _ => {}
}
```

---

## 四、改动量评估

| 模块 | 改动 |
|---|---|
| `inject.rs` | 新增 `inject_keys`、`inject_mouse`、`inject_click`、`inject_scroll` |
| `ws.rs` | JSON 解析 + 分发 |
| 新增 `keymap.rs` | 字符串键名 → `enigo::Key` 的跨平台映射 |
| 新增 `command.rs`（可选） | `Command` 枚举 + serde 反序列化 |
| `static/index.html` | 增加快捷键面板 / 宏按钮区 |
| 测试 | 增加按键序列的单元测试（用 mock 或 trait 抽象） |

核心难点只有一个：**修饰键的跨平台语义**（`"Cmd"` 在 macOS 是 Meta、在 Windows 没对应，要决定是报错还是降级成 Ctrl）。

---

## 五、安全考虑

远程执行任意快捷键 = 远程能做关机、`Win+R cmd`、`Cmd+Space terminal` 这类事。建议至少加：

- **快捷键白名单**：只允许预设的几个组合。
- 或 **二次确认**：手机发请求 → 电脑弹通知 → 用户在 PC 上确认。
- 或 **会话范围限制**：只允许在当前焦点窗口生效（这本来就是 enigo 的默认行为，但要注意防止用户误操作）。

不加防护也可以，但要清楚风险。

---

## 六、推进顺序建议

1. **先按原计划提交 + 推送 + macOS 实测**（验证基础链路：扫码、WebSocket、enigo.text 中文注入）。
2. **再决定要不要扩展快捷键**——基础链路通了再加复杂指令，否则调试时不好定位是网络问题还是指令解析问题。
