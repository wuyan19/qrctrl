//! 输入后端抽象：把 enigo（键鼠注入）+ arboard（剪贴板）的系统副作用
//! 统一到一个 trait 后面，让 `ws::dispatch` 的业务逻辑可脱离真实系统 API 测试。
//!
//! 设计要点：
//! - trait 方法**同步签名**——生产实现是阻塞的（enigo/arboard 都是同步 API），
//!   调用方（`ws::dispatch`）仍用 `spawn_blocking` 调度。Mock 实现同步执行即可，
//!   测试里不需要 runtime。
//! - 错误统一成 `BackendError`，合并原来 inject 的 `String` 和 clipboard 的 `CbError`。
//!   提供 `error_code()` 映射到协议错误码字符串。
//! - 生产实现 `EnigoBackend` 持有 `Arc<Mutex<Enigo>>` + `ClipboardHandle`，
//!   方法体直接转发到 `inject::` / `clipboard::` 现有函数——零行为变更。
//! - `AppState.backend: Arc<dyn InputBackend + Send + Sync>`，测试时注入 `MockBackend`。

use std::path::PathBuf;
use std::sync::Arc;

use enigo::{Axis, Button};

use crate::clipboard::{self, ClipboardHandle};
use crate::inject;

/// trait 对象的统一类型别名：`Arc<dyn InputBackend + Send + Sync>`。
///
/// 所有持有后端的地方（AppState.backend、ws.rs 的 helper 参数）都用这个别名，
/// 避免有的地方写 `dyn InputBackend`、有的写 `dyn InputBackend + Send + Sync` 导致类型不匹配。
/// Send + Sync 是必须的：后端要在 tokio 多线程 runtime + spawn_blocking 之间共享。
pub type DynBackend = Arc<dyn InputBackend + Send + Sync>;

/// 后端操作的统一错误。
///
/// 合并两类来源：
/// - `Inject(String)`：enigo 注入失败（原来 inject::xxx 返回的 `Result<(), String>`）
/// - `Clipboard(CbError)`：剪贴板读写失败（原来 clipboard::xxx 返回的 `Result<_, CbError>`）
///
/// `error_code()` 把它映射成 WS 协议错误码字符串，供 dispatch 生成 error_json。
#[derive(Debug)]
pub enum BackendError {
    // String 通过 Debug derive 被 tracing::warn!(error = ?e) 打印，字段本身不直接读取。
    // 用 #[allow] 抑制 dead_code 误报。
    #[allow(dead_code)]
    Inject(String),
    #[allow(dead_code)]
    Clipboard(clipboard::CbError),
}

impl BackendError {
    /// 映射到 WS 协议错误码。
    /// 注入失败统一 `inject_failed`；剪贴板失败沿用 CbError 的细粒度错误码。
    pub fn error_code(&self) -> &'static str {
        match self {
            BackendError::Inject(_) => "inject_failed",
            BackendError::Clipboard(e) => clipboard::error_code(e),
        }
    }
}

impl From<String> for BackendError {
    fn from(s: String) -> Self {
        BackendError::Inject(s)
    }
}

impl From<clipboard::CbError> for BackendError {
    fn from(e: clipboard::CbError) -> Self {
        BackendError::Clipboard(e)
    }
}

/// 剪贴板里某个文件的元信息（从 clipboard::FileMeta 重新导出，避免测试代码依赖 clipboard 模块细节）。
pub struct FileMeta {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub mime: String,
}

/// 输入后端：把所有系统副作用（键鼠注入 + 剪贴板）收口到一个 trait。
///
/// 生产实现 `EnigoBackend` 转发到 `inject::` / `clipboard::`；
/// 测试实现 `MockBackend`（在 `ws` 模块的 tests 里）记录调用、返回预设结果，
/// 让 `dispatch` 的协议逻辑可以不接触真实 OS 验证。
///
/// 方法命名沿用业务语义（inject_text / read_clipboard_text ...），
/// 而非底层 API（enigo::text / arboard::get_text），这样 trait 不绑定具体库。
pub trait InputBackend: Send + Sync {
    // ---- 键鼠注入 ----
    fn inject_text(&self, text: &str) -> Result<(), BackendError>;
    fn inject_key(&self, key: enigo::Key) -> Result<(), BackendError>;
    fn inject_mouse_move(&self, dx: i32, dy: i32) -> Result<(), BackendError>;
    fn inject_mouse_button(&self, button: Button) -> Result<(), BackendError>;
    fn inject_mouse_button_press(&self, button: Button) -> Result<(), BackendError>;
    fn inject_mouse_button_release(&self, button: Button) -> Result<(), BackendError>;
    fn inject_mouse_scroll(&self, amount: i32, axis: Axis) -> Result<(), BackendError>;
    fn inject_copy(&self) -> Result<(), BackendError>;
    fn inject_paste(&self) -> Result<(), BackendError>;

    // ---- 剪贴板 ----
    /// 读剪贴板文本。`Ok(None)` 表示无文本格式。
    fn read_clipboard_text(&self) -> Result<Option<String>, BackendError>;
    /// 读剪贴板图片，返回 (mime, base64)。`Ok(None)` 表示无图片。
    fn read_clipboard_image(&self) -> Result<Option<(String, String)>, BackendError>;
    /// 把任意格式图片字节解码后写入剪贴板。
    fn write_clipboard_image(&self, bytes: &[u8]) -> Result<(), BackendError>;
    /// 读剪贴板里的文件引用列表。空 Vec 表示无文件。
    fn read_clipboard_files(&self) -> Result<Vec<FileMeta>, BackendError>;

    // ---- trait 对象向下转型（测试用）----
    /// 提供 `Any` 句柄，让测试能把 `dyn InputBackend` downcast 回具体类型（如 MockBackend）
    /// 来读取记录的调用。生产代码不调用此方法。
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any;
}

/// 生产实现：持有 enigo + arboard 句柄，转发到 inject:: / clipboard:: 现有函数。
///
/// 所有方法体都是单行转发——行为与重构前完全一致，只是把调用入口从
/// `state.enigo` + `state.clipboard` 收敛到 `state.backend`。
pub struct EnigoBackend {
    enigo: Arc<parking_lot::Mutex<enigo::Enigo>>,
    clipboard: ClipboardHandle,
}

impl EnigoBackend {
    pub fn new(
        enigo: Arc<parking_lot::Mutex<enigo::Enigo>>,
        clipboard: ClipboardHandle,
    ) -> Self {
        Self { enigo, clipboard }
    }
}

impl InputBackend for EnigoBackend {
    fn inject_text(&self, text: &str) -> Result<(), BackendError> {
        inject::inject_text(&self.enigo, text).map_err(BackendError::Inject)
    }

    fn inject_key(&self, key: enigo::Key) -> Result<(), BackendError> {
        inject::inject_key(&self.enigo, key).map_err(BackendError::Inject)
    }

    fn inject_mouse_move(&self, dx: i32, dy: i32) -> Result<(), BackendError> {
        inject::inject_mouse_move(&self.enigo, dx, dy).map_err(BackendError::Inject)
    }

    fn inject_mouse_button(&self, button: Button) -> Result<(), BackendError> {
        inject::inject_mouse_button(&self.enigo, button).map_err(BackendError::Inject)
    }

    fn inject_mouse_button_press(&self, button: Button) -> Result<(), BackendError> {
        inject::inject_mouse_button_press(&self.enigo, button).map_err(BackendError::Inject)
    }

    fn inject_mouse_button_release(&self, button: Button) -> Result<(), BackendError> {
        inject::inject_mouse_button_release(&self.enigo, button).map_err(BackendError::Inject)
    }

    fn inject_mouse_scroll(&self, amount: i32, axis: Axis) -> Result<(), BackendError> {
        inject::inject_mouse_scroll(&self.enigo, amount, axis).map_err(BackendError::Inject)
    }

    fn inject_copy(&self) -> Result<(), BackendError> {
        inject::inject_copy(&self.enigo).map_err(BackendError::Inject)
    }

    fn inject_paste(&self) -> Result<(), BackendError> {
        inject::inject_paste(&self.enigo).map_err(BackendError::Inject)
    }

    fn read_clipboard_text(&self) -> Result<Option<String>, BackendError> {
        clipboard::read_text(&self.clipboard).map_err(BackendError::Clipboard)
    }

    fn read_clipboard_image(&self) -> Result<Option<(String, String)>, BackendError> {
        clipboard::read_image_png_base64(&self.clipboard).map_err(BackendError::Clipboard)
    }

    fn write_clipboard_image(&self, bytes: &[u8]) -> Result<(), BackendError> {
        clipboard::write_image_from_bytes(&self.clipboard, bytes).map_err(BackendError::Clipboard)
    }

    fn read_clipboard_files(&self) -> Result<Vec<FileMeta>, BackendError> {
        clipboard::read_file_list(&self.clipboard)
            .map_err(BackendError::Clipboard)
            .map(|files| {
                files
                    .into_iter()
                    .map(|fm| FileMeta {
                        path: fm.path,
                        name: fm.name,
                        size: fm.size,
                        mime: fm.mime,
                    })
                    .collect()
            })
    }

    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
