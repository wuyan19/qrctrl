# 未来扩展设计草稿

本文档汇总 **尚未实现** 的功能设计。`research.md` 和 `research2.md` 中已落地的部分已删除，只保留仍在 roadmap 上的内容。

当前已实现的能力一览（用于对照）：文本注入、双向文本/图片剪贴板、双向任意文件传输（PC→手机走 `Cmd+C` 文件 + `GetFile`；手机→PC 走 `/upload`）、鼠标移动/点击/tap-tap-drag/滚轮、Enter/Tab/Backspace/Copy/Paste、JSON 协议、`--token` / `--prefer-ip` / `--save-dir` / `--max-size`、系统托盘 + GUI 子系统后台运行。

---

## 一、快捷键序列

### 1.1 现状

目前只支持单键（Enter / Tab / Backspace / Copy / Paste）。复杂的修饰键组合（`Cmd+Space`、`Win+R`、`Cmd+Tab`、`Alt+F4`）尚未实现。

### 1.2 协议设计草稿

```json
{"type":"keys","sequence":["Cmd","Space"]}
{"type":"keys","sequence":["Ctrl","Shift","Esc"]}
```

后端反序列化为 `Vec<Key>` 后逐个 Press，最后一个 Click，再倒序 Release。`Key` 用字符串，跨平台映射到 `enigo::Key`。

### 1.3 关键难点：修饰键跨平台语义

| 字符串 | macOS | Windows | Linux |
|---|---|---|---|
| `Cmd` | Meta（实际就是 ⌘） | **无对应键** | Super |
| `Win` | 无对应键 | Meta | Super |
| `Ctrl` | Control | Control | Control |
| `Alt` | Option | Alt | Alt |

核心决策：`Cmd` 在 Windows 上是 **报错** 还是 **降级成 Ctrl**？建议：

- **报错** 更安全（防止用户以为生效了其实没有）
- **降级** 更友好（同一份快捷键配置在多平台能用）

折中：协议区分 `Meta`（语义中性，跟随平台 = `Cmd`/`Win`/`Super`）和显式 `Cmd`/`Win`（平台特定，跨平台报错）。

### 1.4 安全考虑（必读）

**远程执行任意快捷键 = 远程能关机、远程能 `Win+R cmd`、远程能 `Cmd+Space terminal`。**

不加防护的话，攻击者一旦拿到 token（局域网抓包、URL 截图泄露），就能完全接管 PC。建议至少加一层：

| 方案 | 描述 | 适用场景 |
|---|---|---|
| **白名单** | 只允许预设几个组合（如 `Cmd+Space` / `Cmd+Tab` / 播放控制） | 个人用户，简单 |
| **二次确认** | 手机发请求 → PC 弹通知 → 用户在 PC 上确认 | 严格控制，体验差 |
| **会话范围限制** | 只对当前焦点窗口生效（enigo 默认行为，但无法防止恶意组合键） | 兜底 |

推荐**白名单**。提供 `--allow-shortcuts <list>` CLI 参数让用户自定义；默认不开。

### 1.5 实施改动

- `inject.rs`：新增 `inject_keys(enigo, &[Key])`
- 新增 `keymap.rs`：字符串 → `enigo::Key` 的跨平台映射
- `ws.rs`：`Command::Keys { sequence: Vec<String> }`
- 前端：增加宏按钮区 + 自定义快捷键编辑器
- 单测：mock enigo trait 抽象（参考 `tests/send_sync_invariants.rs`）

---

## 二、Metadata + Blob 架构（重大重构，非必需）

### 2.1 当前实现的局限

当前每个内容类型走独立通道：
- 文本：WS JSON
- 图片：WS base64 over JSON
- 文件：HTTP `/upload` `/download`，用 UUID 标识

简单直接，但**扩展困难**：
- 想加「剪贴板历史」？要在三种通道各自做版本管理
- 想加「多设备同步」？三个通道的状态模型不一致
- 想加「缩略图」？图片通道要单独加
- 想加「断点续传」？要改 `/download` 协议
- 想加「内容去重」？UUID 无法识别重复内容

### 2.2 提议的统一抽象

```rust
struct ClipboardEntry {
    id: String,
    timestamp: u64,
    content: Content,
}

enum Content {
    Text(TextContent),
    Image(ImageContent),
    File(FileContent),
}

struct EntryMeta {
    id: String,
    kind: ContentKind,
    size: u64,
    created_at: u64,
}

struct Blob {
    id: String,    // SHA256(data)
    size: u64,
}
```

存储布局（类 Git 风格，前两位分桶）：

```text
data/
├── metadata/
│   ├── 1.json
│   └── 2.json
└── blobs/
    ├── a3/
    │   └── a3c7f8...
    └── b5/
        └── b58f...
```

### 2.3 控制面 / 数据面分离（**这一点当前实现已采纳**）

```text
WebSocket  →  控制面（命令、元数据同步、状态）
HTTP       →  数据面（blob 下载、缩略图、断点续传）
```

`/upload` / `/download` 已经走 HTTP，命令走 WS —— 这是 research2.md 唯一已采纳的部分。

### 2.4 Blob 下载协议（HTTP Range 续传）

```http
GET /blob/{sha256}
Range: bytes=1048576-
```

天然支持：
- 断点续传
- 下载进度
- 大文件（GB 级）

### 2.5 缩略图策略

```rust
struct ImageContent {
    width: u32,
    height: u32,
    thumbnail_blob: String,  // 小图（< 200x200）
    original_blob: String,
}
```

手机端：
- 列表显示缩略图（小、快）
- 点击下载原图

阈值 1MB 以内的图：直接返回原图，不生成缩略图。

### 2.6 何时回头考虑这个重构

| 触发条件 | 重构必要性 |
|---|---|
| 用户要求剪贴板历史 | 高 |
| 用户要求多设备同步 | 高 |
| 用户要求缩略图浏览 | 中（可只对图片通道单独加） |
| 单文件超过 ~500MB，需要续传 | 中（可只对 `/download` 加 Range） |
| 当前使用场景稳定 | **不需要重构** |

**建议**：除非出现明确的历史/同步需求，否则保持当前三通道实现。Research2.md 的设计**仅在需要长期演进时**才有价值。

---

## 三、TLS / HTTPS

当前 HTTP 明文，局域网内可用但**不受信任网络下不安全**（token 可被嗅探）。

实现选项：
- **自签证书**：iOS Safari 上 `wss://` 有严重坑（首次警告 + 证书过期全员重做）
- **mkcert 本地 CA**：PC 装 CA 没用，手机端要单独装到 iOS 钰匙串 / Android 系统 CA，普通用户搞不定
- **Tailscale / WireGuard**：在网络层加密，不动应用层 —— **推荐**

短期不实施，README 里提示「仅用于受信任局域网」。

---

## 四、宏按钮 / 自定义面板

让用户在前端自定义一组按钮，每个按钮绑定一段文本/快捷键/命令序列。配置存 PC 端（如 `~/.config/qrctrl/macros.json`），通过 WS 同步给手机。

简单实现：CLI `--macros <path>` + WS 推送 `{"type":"macros","list":[...]}`。复杂实现：手机端 UI 编辑器。

---

## 五、参考推进顺序

1. **快捷键序列（带白名单）** —— 最常被要求的功能
2. **TLS（如出现远程使用需求）** —— 优先 Tailscale 而非自签
3. **宏按钮** —— 用户体验提升
4. **Metadata + Blob 重构** —— 仅当剪贴板历史/多设备同步成为真实需求
