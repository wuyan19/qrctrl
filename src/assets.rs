//! 编译期嵌入 `static/` 目录所有文件，运行时按 URL 路径 serve。
//!
//! 之前只用 `include_str!` 内联 index.html / config.html；拆出 CSS/JS 后改用
//! rust-embed 统一处理——新增静态文件不用改代码。HTML 仍由 main.rs / config.rs
//! 自己读取后做 `__THEME__` / `__DEVICE_NAME__` 占位符替换。

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "static/"]
struct Asset;

/// 读嵌入文件为 UTF-8 字符串。HTML handler 用这个 + 自己做模板替换。
///
/// Cargo.toml 里启用了 `debug-embed` feature，所以 debug / release 都是编译期嵌入，
/// `f.data` 永远是 `Cow::Borrowed(&'static [u8])`——直接拿内部引用返回即可。
pub fn read_str(name: &str) -> Option<&'static str> {
    let f = Asset::get(name)?;
    let bytes: &'static [u8] = match f.data {
        std::borrow::Cow::Borrowed(b) => b,
        std::borrow::Cow::Owned(_) => return None,
    };
    std::str::from_utf8(bytes).ok()
}

/// `GET /css/{*path}` / `GET /js/{*path}` → 静态资源。
/// 路由前缀（css/、js/）只用来分流；handler 用完整 URL 路径查嵌入表，
/// 这样 css/js 子目录的子路径也能直接命中（如 `/css/sub/foo.css`）。
pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    match Asset::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref())],
                f.data.into_owned(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
