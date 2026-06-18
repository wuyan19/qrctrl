use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use parking_lot::Mutex;
use serde::Deserialize;
use tao::event_loop::EventLoopProxy;
use tokio::sync::Notify;

use crate::backend::InputBackend;
use crate::file_transfer::TransferRegistry;
use crate::tray::UserEvent;

/// HTTP / WebSocket 共用的 token 查询参数。
#[derive(Deserialize)]
pub struct TokenQuery {
    pub t: String,
}

/// 鉴权 marker：实现了 `FromRequestParts`，校验 `?t=<token>` 与 `state.token` 一致。
///
/// 在 handler 签名里用 `_: Authed` 作为参数即可触发鉴权，把原本散落在 14 个 handler 里
/// 的 `if q.t != state.token { return Err(UNAUTHORIZED) }` 收敛到一处。
///
/// 它不「消费」query string——内部用 `Query::<TokenQuery>` 提取（Query extractor 可重复
/// 调用，每次都从同一个 URI 重新解析），所以 handler 里仍然可以再写 `Query<TokenQuery>`
/// 或 `Query<ListDirQuery>`（后者 `#[serde(flatten)]` 携带 token + 业务字段）。
///
/// 提取顺序很重要：Authed 必须排在 State 之前，因为 FromRequestParts 按参数顺序执行，
/// 而 State<AppState> 也是从 parts 提取（通过 Extension）。实际两者无依赖，顺序不限。
pub struct Authed;

/// 校验失败时返回的简化错误响应，带 plain text body 便于排查。
type AuthRejection = (axum::http::StatusCode, &'static str);

impl FromRequestParts<AppState> for Authed {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        use axum::http::StatusCode;
        // Query 提取失败（缺 ?t= 或非法编码）当作未授权。
        // Query extractor 可重复调用（每次都从 URI 重新解析），不消费 parts，
        // 所以 handler 里仍能再写 Query<TokenQuery> 或 Query<ListDirQuery>。
        let q = axum::extract::Query::<TokenQuery>::from_request_parts(parts, state)
            .await
            .map_err(|_| (StatusCode::UNAUTHORIZED, "unauthorized"))?;
        if q.t != state.token {
            return Err((StatusCode::UNAUTHORIZED, "unauthorized"));
        }
        Ok(Authed)
    }
}

#[derive(Clone)]
pub struct AppState {
    pub token: String,
    pub name: String,
    pub addr: String,
    pub port: u16,
    pub prefer_ip: Option<String>,
    /// 输入后端：键鼠注入 + 剪贴板的抽象 trait 对象。
    /// 生产用 `EnigoBackend`（转发到 inject:: / clipboard::），测试用 `MockBackend`。
    /// 用 trait 对象而非泛型，让 AppState 保持 Clone + 无泛型参数，axum handler 签名不变。
    pub backend: Arc<dyn InputBackend + Send + Sync>,
    pub save_dir: PathBuf,
    pub max_size: u64,
    pub registry: TransferRegistry,
    /// 触发 server + tray 优雅 shutdown。restart 和 quit 都通过它发起。
    pub shutdown_notify: Arc<Notify>,
    /// 给 server 线程用来唤醒 tao event loop（restart 信号需要让 tray loop 退出）。
    pub tray_proxy: EventLoopProxy<UserEvent>,
    /// UI 主题偏好：`"dark"` / `"light"` / `"system"`。
    /// 用 `Arc<Mutex<String>>` 是因为 theme 走 live-apply——前端切换按钮 POST /api/theme
    /// 后立即改这个值，所有后续 ws server_info 推送都会带新 theme。其他配置字段都不需要
    /// 运行时可变（改了也只写文件、等重启生效）。
    pub theme: Arc<Mutex<String>>,
    /// 触控板灵敏度倍数（默认 1.0）。ws dispatch MouseMove 时把 dx/dy 乘以该值再注入后端。
    /// 同 theme 走 live-apply：前端配置页 range slider onchange POST /api/mouse_sensitivity
    /// 立即改这里，下一次 mouse_move 就用新值。
    pub mouse_sensitivity: Arc<Mutex<f32>>,
}
