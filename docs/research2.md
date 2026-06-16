结合你前面提到的需求：

* 手机扫码连接 PC
* 手机端主动发起所有操作（PC 不主动推送）
* 支持文本、图片、文件
* 图片需要预览
* 文件可能很大（GB 级）
* 后续可能支持历史记录
* Rust 开发

我建议采用 **元数据（Metadata）+ Blob（大对象）** 的架构，而不是剪贴板内容直接同步。

---

# 总体架构

```text
┌─────────────┐
│   Android   │
└──────┬──────┘
       │
       │ WebSocket
       │
┌──────▼──────┐
│   Rust PC   │
└─────────────┘

        ↓

┌─────────────────┐
│ Clipboard Store │
└─────────────────┘
         │
         ├── Metadata
         └── Blob
```

---

# 核心抽象

不要按：

```rust
Text
Image
File
```

设计协议。

而是：

```rust
struct ClipboardEntry {
    id: String,
    timestamp: u64,
    content: Content,
}
```

---

Content：

```rust
enum Content {
    Text(TextContent),
    Image(ImageContent),
    File(FileContent),
}
```

---

# 元数据层

所有同步都只传 Metadata。

```rust
struct EntryMeta {
    id: String,
    kind: ContentKind,
    size: u64,
    created_at: u64,
}
```

---

文本：

```rust
struct TextContent {
    text: String,
}
```

---

图片：

```rust
struct ImageContent {
    width: u32,
    height: u32,

    thumbnail_blob: String,
    original_blob: String,
}
```

---

文件：

```rust
struct FileContent {
    name: String,
    mime: String,

    blob_id: String,
}
```

---

# Blob 存储层

所有大对象统一处理：

```rust
struct Blob {
    id: String,
    size: u64,
}
```

其中：

```text
id = SHA256(data)
```

例如：

```text
a3c7f8...
```

---

目录结构：

```text
data/

├── metadata/
│   ├── 1.json
│   ├── 2.json
│   └── ...
│
└── blobs/
    ├── a3/
    │   └── a3c7f8...
    │
    ├── b5/
    │   └── b58f...
```

类似 Git。

---

# 手机获取剪贴板

手机主动：

```http
GET /clipboard/latest
```

返回：

```json
{
  "id":"123",
  "type":"image",
  "width":1920,
  "height":1080,
  "size":5321131,

  "thumbnail_blob":"abc",
  "original_blob":"def"
}
```

---

文本直接返回：

```json
{
  "type":"text",
  "text":"hello world"
}
```

因为文本通常很小。

---

# 图片策略

## 小图

阈值：

```rust
1MB
```

以内：

```text
直接返回
```

---

## 大图

返回：

```text
缩略图 Blob
原图 Blob
```

---

手机显示：

```text
图片预览
↓
点击
↓
下载原图
```

---

# 文件策略

复制文件时：

```text
CF_HDROP
```

获得：

```text
D:\movie.mkv
```

---

PC 端记录：

```json
{
  "type":"file",
  "name":"movie.mkv",
  "size":9348572398,
  "blob_id":"xxx"
}
```

---

手机显示：

```text
🎬 movie.mkv
8.7GB
```

---

点击下载：

```http
GET /blob/xxx
```

才开始传输。

---

# Blob 下载

不要 Base64。

直接：

```text
HTTP Range
```

或

```text
WebSocket Binary
```

---

推荐：

```http
GET /blob/{id}
```

支持：

```http
Range: bytes=1048576-
```

这样天然支持：

* 断点续传
* 下载进度
* 大文件

---

# 传输协议

WebSocket 保持控制连接：

```text
连接
心跳
同步状态
命令
```

---

例如：

```json
{
  "cmd":"get_latest"
}
```

---

```json
{
  "cmd":"copy_text",
  "text":"hello"
}
```

---

```json
{
  "cmd":"copy_image",
  "blob":"abc"
}
```

---

真正的大数据：

```text
HTTP
```

传输。

不要走 WebSocket。

---

# 为什么这样设计

很多人喜欢：

```text
WebSocket
传所有东西
```

最后会发现：

```text
10GB 文件
↓
WebSocket
↓
内存爆炸
↓
重连困难
↓
断点续传麻烦
```

---

而：

```text
WebSocket
    ↓
控制面(Control Plane)

HTTP
    ↓
数据面(Data Plane)
```

是成熟产品常见方案。

---

# 最终建议

对于你的项目，我会采用：

```text
控制层
========
WebSocket

用途：
- 手机连接 PC
- 剪贴板状态同步
- 命令下发
- 元数据同步

数据层
========
HTTP

用途：
- 图片下载
- 文件下载
- 缩略图下载
- 断点续传

存储层
========
Metadata + Blob

文本
    直接存 Metadata

图片
    Metadata + Blob

文件
    Metadata + Blob

Blob ID
    SHA256
```

这个架构后面扩展：

* 剪贴板历史
* 收藏
* 最近文件
* 多设备同步
* 图片预览
* 文件断点续传

都不需要推翻重做。对于你这个项目规模来说，已经足够长期使用了。
