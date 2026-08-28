//! 编译期嵌入 `static/` 目录所有文件，运行时按 URL 路径 serve；HTML 页面
//! handler（读嵌入模板 + 占位符替换）也在这里——「静态资源与模板」是同一个职责。
//!
//! 之前只用 `include_str!` 内联 index.html / config.html；拆出 CSS/JS 后改用
//! rust-embed 统一处理——新增静态文件不用改代码。

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::state::AppState;

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

/// 读嵌入 HTML 模板并做首屏占位符替换。
///
/// 主题占位符 `data-theme="__THEME__"` 读 `state.core.theme`（可能被
/// `set_theme_handler` 在运行时改过）。inline `<script>` 会同步把 `"system"`
/// 解析成 dark/light 应用到 `<html>`，避免 CSS 应用后的 FOUC。
/// `title_placeholder` 是 `<title>__DEVICE_NAME__</title>` 这类页面特有占位符，
/// 没有的页面传 None。缺文件直接 panic——嵌入表编译期定死，运行时不可能缺。
fn render_page(state: &AppState, file: &str, title_placeholder: Option<&str>) -> Html<String> {
    let theme = state.core.theme.lock().clone();
    let mut html = read_str(file)
        .unwrap_or_else(|| panic!("{} 编译期嵌入，运行时一定存在", file))
        .replace(
            "data-theme=\"__THEME__\"",
            &format!("data-theme=\"{}\"", theme),
        );
    if let Some(placeholder) = title_placeholder {
        let name = escape_html(&state.core.name);
        html = html.replace(placeholder, &format!("<title>{}</title>", name));
    }
    Html(html)
}

/// 把设备名里的 HTML 元字符转义，避免 `<title>` 注入。设备名来自 CLI / config.toml /
/// hostname，CLI 和文件来源没有限制字符集，所以这条 escape 是必要的（虽然 hostname 几乎
/// 不会含这些字符）。theme 字段已过 `normalize_theme` 校验只能是三个固定字符串，无需 escape。
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// `GET /` → index.html（手机控制面板首页）。
pub async fn index_handler(State(state): State<AppState>) -> Html<String> {
    render_page(&state, "index.html", Some("<title>__DEVICE_NAME__</title>"))
}

/// `GET /config?t=<token>` → 配置页 HTML。
/// token 校验由 `Authed` extractor 完成（401），handler 只关心业务。
pub async fn config_page_handler(
    _: crate::state::Authed,
    State(state): State<AppState>,
) -> Html<String> {
    render_page(&state, "config.html", None)
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
