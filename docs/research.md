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

---

## 七、反向通道：PC → 手机（剪贴板同步）

### 7.1 使用场景

在 PC 上复制内容（文本/图片/文件）后，希望在手机端快速拿到。

### 7.2 触发模式：Pull 模式

**手机端主动拉取**：UI 增加「📋 获取粘贴板」按钮，点击后向 PC 发请求，PC 返回当前剪贴板内容。

**优点**（相比 push 模式）：
- 不需要 PC 端轮询线程
- 隐私友好：用户主动触发，敏感内容不会被推送
- 资源节省

### 7.3 协议设计

**请求**（手机 → PC）：
```json
{"type":"get_clipboard"}
```

**响应 - 文本**（PC → 手机）：
```json
{"type":"clipboard","kind":"text","content":"复制的文本"}
```

**响应 - 图片**（PC → 手机，base64 编码）：
```json
{"type":"clipboard","kind":"image","mime":"image/png","data":"iVBORw0KGgo..."}
```

**响应 - 文件**（PC → 手机，base64 编码）：
```json
{"type":"clipboard","kind":"file","filename":"notes.txt","mime":"text/plain","size":1024,"data":"base64..."}
```

**错误**：
```json
{"type":"clipboard_error","error":"empty | unsupported | read_failed"}
```

### 7.4 前端 UI 行为（按 kind 分发）

| kind | 处理流程 |
|---|---|
| **text** | ① 尝试 `navigator.clipboard.writeText`<br>② 成功 → toast「已复制到剪贴板」<br>③ 失败（HTTP 限制）→ **写入 textarea + `select()` 全选**，toast「已写入输入框，点复制按钮即可」 |
| **image** | base64 → blob → objectURL，弹出图片预览模态。用户**长按图片**即可保存/复制（iOS/Android 原生行为）。可附带「下载」按钮 |
| **file** | 弹出文件模态：文件名 + 大小 + 「下载」按钮（用 `<a download>` 触发浏览器下载） |
| **error** | toast 错误信息（如「剪贴板为空」「剪贴板内容不支持」） |

### 7.5 关于 HTTPS（重要结论：不迁移）

**结论：保持 HTTP，用 `document.execCommand('copy')` 降级方案。**

理由：
- 局域网 IP 无法从公共 CA 申请证书
- `mkcert` 本地 CA 装在 PC 上，手机端不信任，要单独装到 iOS 钥匙串 / Android 系统 CA，普通用户搞不定
- 自签证书在 iOS Safari 上有严重坑：① 首次访问要手动绕过警告 ② **wss://**（WebSocket over TLS）自签在 iOS Safari 上经常直接失败 ③ 证书过期后所有手机要重做
- HTTPS 解决不了根本问题：`navigator.clipboard.writeText()` 在新版 iOS 上仍需用户手势触发，且 HTTP 下 `navigator.clipboard` 直接是 `undefined`

**降级方案**：`document.execCommand('copy')`（deprecated 但所有手机浏览器实际还支持，HTTP 下也能工作）：

```javascript
async function copyToClipboard(text) {
  // 优先尝试现代 API（仅在 HTTPS / localhost 下生效）
  if (navigator.clipboard && window.isSecureContext) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {}
  }
  // 降级：execCommand
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.focus();
  ta.select();
  try {
    const ok = document.execCommand('copy');
    document.body.removeChild(ta);
    return ok;
  } catch {
    document.body.removeChild(ta);
    return false;
  }
}
```

最后兜底（两个 API 都失败时）：显示文本在原输入框（textarea）里 + 自动 `select()` 全选，提示用户「点复制按钮」或「长按文本手动复制」。

### 7.6 技术选型

| 组件 | 选项 | 备注 |
|---|---|---|
| **剪贴板读取库** | `arboard` | 跨平台（Win/Mac/Linux），活跃维护，支持文本 + 图片 |
| **PNG 编码** | `png` crate | 比 `image` 轻量很多，只需 RGBA → PNG |
| **Base64** | `base64` crate | 标准库 |
| **传输方式** | base64 over JSON | 局域网小文件场景，简单优先 |

### 7.7 实施分阶段

| 阶段 | 范围 | 工作量 |
|---|---|---|
| **阶段 1（MVP）** | 文本读取 + 前端 textarea 兜底 | ~3 小时 |
| **阶段 2** | 图片读取 + PNG 编码 + 图片预览模态 | ~3 小时 |
| **阶段 3** | 文件读取（需要平台特定代码） | ~1 天 |

### 7.8 大小限制

| 类型 | 上限 | 超出处理 |
|---|---|---|
| 文本 | 100 KB | 截断 + 提示 |
| 图片 | 5 MB | 拒绝 + 提示 |
| 文件 | 10 MB | 拒绝 + 提示 |

axum WebSocket 默认消息上限 64 MB，可放行。

### 7.9 关键改造点（实施时参考）

- **`ws.rs`**：当前是 `socket.recv()` 单向循环，要改造成 `tokio::select!` 双向循环（手机 → PC 现有 + PC → 手机新增）。但因为 pull 模式下 PC 响应就是当前请求的回包，可以直接在 `socket.recv()` 后用 `socket.send()` 回复，不需要 mpsc 通道。
- **新增 `clipboard.rs`**：封装 `arboard` 调用，提供 `read_text()` / `read_image()`，都要在 `spawn_blocking` 中调用（阻塞 API）。
- **新增 `command.rs`**（可选）：定义 `Command` 枚举 + serde 反序列化。
- **前端 `static/index.html`**：增加按钮 + 模态框 + `copyToClipboard` 工具函数 + 图片预览组件。

---

## 八、手机 → PC 图片/文件推送

### 8.1 使用场景

把手机上的截图、相册图片、文件推送到 PC。最常见用法：手机截图后粘到 PC 上正在编辑的文档、聊天窗口、设计稿里。

### 8.2 协议设计

**请求**（手机 → PC）：
```json
{"type":"set_clipboard","kind":"image","mime":"image/png","data":"base64..."}
```

**成功响应**：
```json
{"type":"set_clipboard_ok"}
```

**失败响应**：
```json
{"type":"set_clipboard_error","error":"decode_failed | write_failed | too_large"}
```

### 8.3 手机端选图（HTML 原生，无坑）

`<input type="file" accept="image/*">` 在手机上弹出「相机 / 相册」选项，**HTTP 下完全可用**（不像 Clipboard API 需要 HTTPS）。这是这个方向比反向简单的关键原因。

```javascript
const input = document.createElement('input');
input.type = 'file';
input.accept = 'image/*';
input.onchange = async () => {
  const file = input.files[0];
  const buf = await file.arrayBuffer();
  const b64 = btoa(String.fromCharCode(...new Uint8Array(buf)));
  ws.send(JSON.stringify({
    type: 'set_clipboard',
    kind: 'image',
    mime: file.type,
    data: b64,
  }));
};
input.click();
```

### 8.4 PC 端写入剪贴板

`arboard::set_image` 接收 RGBA 像素，需要先用 `image` crate 把 PNG/JPEG 解码为 RGBA：

```rust
use image::GenericImageView;

let bytes = base64::decode(&data)?;
let decoded = image::load_from_memory(&bytes)?;
let (w, h) = decoded.dimensions();
let rgba = decoded.to_rgba8().into_raw();

cb.set_image(arboard::ImageData {
    width: w as usize,
    height: h as usize,
    rgba: rgba.into(),
})?;
```

**跨平台一致效果**：
- macOS：进入 NSPasteboard，`Cmd+V` 粘到微信 / Preview / 备忘录
- Windows：CF_DIB 进入剪贴板，`Ctrl+V` 粘到画图 / Word / 任意应用
- Linux：X11 clipboard 的 image/png target

### 8.5 与 PC → 手机 方向对比

| 维度 | 手机 → PC（本节） | PC → 手机（第七节） |
|---|---|---|
| 触发方式 | 手机端选文件按钮 | 手机端拉取按钮 |
| 协议方向 | 上行（set_clipboard） | 下行（get_clipboard → 响应） |
| 前端复杂度 | 简单（file input） | 复杂（execCommand + textarea 兜底 + 模态预览） |
| 后端复杂度 | 中等（解码 + set_image） | 中等（get_image + 编码） |
| HTTPS 依赖 | **无**（file input 在 HTTP 下也工作） | 无（用 execCommand 降级） |
| 浏览器坑 | **基本没有** | 一堆（navigator.clipboard 在 HTTP 下不可用） |

### 8.6 关键语义决策：剪贴板 vs 文件夹

**「推送图片到 PC 剪贴板」与「直接保存到 PC 文件夹」是两件事**：

| 选项 | 行为 | 适用场景 |
|---|---|---|
| **写入剪贴板** | `Cmd+V` 粘到当前焦点窗口 | 把手机截图粘到聊天/文档 |
| **保存到文件夹** | 落地到 `~/Downloads/`（或 PC 端配置目录） | 备份手机照片、文件归档 |
| **两者都做** | 同时写入剪贴板 + 落地文件 | 最灵活，但实现复杂 |

**推荐方案**：**写入剪贴板**为主，文件落地作为可选行为（启动参数 `--save-dir <path>` 开启）。

### 8.7 大小限制 & 体验

- 图片通常 1-5 MB，base64 后 1.3-7 MB，局域网 < 1 秒
- 大图（> 10 MB）要加进度条（WebSocket 单条消息发送时无法做进度反馈，前端发前显示「上传中…」即可）
- 上限建议：10 MB（base64 后约 13 MB，axum 默认上限 64 MB 远够）

### 8.8 技术选型

| 组件 | 选项 | 备注 |
|---|---|---|
| **剪贴板写入库** | `arboard`（同第七节） | 已选 |
| **图片解码** | `image` crate | 通用解码器（PNG / JPEG / WebP / GIF） |
| **Base64** | `base64` crate | 同第七节 |
| **传输方式** | base64 over JSON | 简单优先，大文件后续可改二进制 |

### 8.9 后续扩展：任意文件推送

图片之外的文件（PDF、文档、压缩包等），剪贴板写入语义模糊（Windows 有 CF_HDROP，macOS 有 NSFilePromiseProvider，跨平台不一致）。第一版**只做图片**，文件落地（`--save-dir`）作为独立 feature 后续考虑。

### 8.10 实施工作量

| 模块 | 工作量 |
|---|---|
| 加 `arboard` + `image` + `base64` 依赖 | 10 分钟 |
| 后端 `clipboard.rs` 增加 `set_image_from_base64` | 1 小时 |
| `ws.rs` 协议分发（识别 `set_clipboard`） | 1 小时 |
| 前端文件选择按钮 + 状态提示 | 1 小时 |
| 大图测试 / 错误处理 | 1 小时 |

整体：约 4 小时。
