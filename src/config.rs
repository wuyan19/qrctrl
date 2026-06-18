//! 配置文件 + 配置页 HTTP handlers。
//!
//! 设计要点：
//! - 配置文件位置：`dirs::config_dir()/qrctrl/config.toml`，TOML 格式
//! - 所有字段 `Option<T>`，`None` 表示「未设置」（用于三层叠加：built-in → 文件 → CLI）
//! - 损坏文件**绝不 panic**（tray app 双击启动下 panic = 静默崩溃），改名 `.bad-{ts}`
//!   备份后用 default 继续
//! - 所有 handler 走 `Query<TokenQuery>` 验证 token，复用现有鉴权机制
//! - `POST /api/config` 通过 axum `Json<T>` extractor 强制 `Content-Type: application/json`
//!   ——浏览器跨站 POST 这个 Content-Type 触发 CORS preflight，我们不开 CORS，等于免费 CSRF 防御

use std::path::PathBuf;

use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::net;
use crate::state::{AppState, TokenQuery};

const CONFIG_HTML: &str = include_str!("../static/config.html");
const ONE_TB: u64 = 1024 * 1024 * 1024 * 1024;

/// 配置文件内容。全部 `Option<T>`，`None` = 未设置（让下层默认生效）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub addr: Option<String>,
    pub port: Option<u16>,
    pub name: Option<String>,
    pub save_dir: Option<PathBuf>,
    pub max_size: Option<u64>,
    pub token: Option<String>,
    pub prefer_ip: Option<String>,
    /// 主题偏好：`"dark"` / `"light"` / `"system"`。None = 跟随系统。
    /// 前端 index.html 通过 ws server_info 拿到后用 `[data-theme]` 应用。
    /// 与其他字段不同：theme 走 live-apply（POST /api/theme 立即改 state + 写文件），
    /// 因为切换主题不该要求用户重启——重启语义是给「真正影响 server 启动」的字段用的。
    pub theme: Option<String>,

    /// 触控板灵敏度倍数（默认 1.0）。后端 ws dispatch 收到 mouse_move 时
    /// 把 dx/dy 乘以该值再注入 enigo，前端完全不感知。
    /// 同 theme 一样走 live-apply（POST /api/mouse_sensitivity）：改 state + 写文件，
    /// 不需要重启。范围 0.1-5.0，过低触控板太肉、过高难以精准点击。
    pub mouse_sensitivity: Option<f32>,
}

/// 返回配置文件路径。`dirs::config_dir()` 在某些嵌入式环境可能返回 None，做兜底。
pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("qrctrl").join("config.toml"))
}

/// 加载配置文件。文件不存在 = `Config::default()`（所有字段 None）。
/// 解析失败 = 把原文件备份成 `config.toml.bad-{timestamp}`，返回 default。
/// 任何阶段都**不 panic**。
pub fn load() -> Config {
    let path = match config_path() {
        Some(p) => p,
        None => {
            eprintln!("[config] 系统未提供 config_dir，跳过配置文件");
            return Config::default();
        }
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        Err(e) => {
            eprintln!("[config] 读取 {} 失败：{}，跳过配置文件", path.display(), e);
            return Config::default();
        }
    };
    match toml::from_str::<Config>(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[config] 解析 {} 失败：{}", path.display(), e);
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = path.with_extension(format!("toml.bad-{}", ts));
            if std::fs::rename(&path, &backup).is_ok() {
                eprintln!("[config] 原文件已备份到 {}", backup.display());
            }
            Config::default()
        }
    }
}

/// 写入配置文件。自动创建父目录。直接 write（非原子）——小 TOML 文件可接受。
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "系统未提供 config_dir")
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, text)
}

/// 字段级校验。空字段（None）一律放行——代表「让下层默认生效」。
pub fn validate(cfg: &Config) -> Result<(), String> {
    if let Some(p) = cfg.port {
        if p == 0 {
            return Err("端口不能为 0".into());
        }
    }
    if let Some(ref addr) = cfg.addr {
        if addr.trim().is_empty() {
            return Err("监听地址不能为空".into());
        }
    }
    if let Some(s) = cfg.max_size {
        if s == 0 {
            return Err("单文件上限必须大于 0".into());
        }
        if s > ONE_TB {
            return Err("单文件上限不能超过 1 TB".into());
        }
    }
    if let Some(ref t) = cfg.token {
        if let Err(e) = crate::token::validate_token(t) {
            return Err(format!("token 不合法：{}", e));
        }
    }
    if let Some(ref theme) = cfg.theme {
        let t = theme.trim().to_lowercase();
        if !matches!(t.as_str(), "dark" | "light" | "system") {
            return Err("theme 必须是 dark / light / system".into());
        }
    }
    if let Some(s) = cfg.mouse_sensitivity {
        if !s.is_finite() || s < 0.1 || s > 5.0 {
            return Err("mouse_sensitivity 必须是 0.1-5.0 之间的有限数".into());
        }
    }
    Ok(())
}

/// 校验并归一化 theme 字符串（trim + lowercase + 必须是合法三选一）。
/// `set_theme_handler` 单独走这条路径，因为它要 live apply + 写文件，
/// 不能依赖 Config 反序列化（前端可能传非法值）。
pub fn normalize_theme(raw: &str) -> Result<String, String> {
    let t = raw.trim().to_lowercase();
    if matches!(t.as_str(), "dark" | "light" | "system") {
        Ok(t)
    } else {
        Err("theme 必须是 dark / light / system".into())
    }
}

// ============================================================================
// HTTP handlers
// ============================================================================

/// `GET /config?t=<token>` → 配置页 HTML，首屏注入当前主题（同 index_handler）。
/// token 校验失败返回 401。
pub async fn config_page_handler(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> Result<Html<String>, axum::http::StatusCode> {
    if q.t != state.token {
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    }
    let theme = state.theme.lock().unwrap().clone();
    let html = CONFIG_HTML.replace(
        "data-theme=\"__THEME__\"",
        &format!("data-theme=\"{}\"", theme),
    );
    Ok(Html(html))
}

/// `GET /api/config?t=<token>` → 当前生效配置 JSON。
pub async fn get_config_handler(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    if q.t != state.token {
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    }
    Ok(Json(json!({
        "addr": state.addr,
        "port": state.port,
        "name": state.name,
        "save_dir": state.save_dir.to_string_lossy(),
        "max_size": state.max_size,
        "token": state.token,
        "prefer_ip": state.prefer_ip,
        "theme": *state.theme.lock().unwrap(),
        "mouse_sensitivity": *state.mouse_sensitivity.lock().unwrap(),
    })))
}

/// `POST /api/config?t=<token>` body=JSON。axum `Json<T>` 强制 Content-Type: application/json，
/// 等同免费 CSRF 防御（浏览器跨站 POST 该 Content-Type 走 CORS preflight，我们不开 CORS）。
///
/// 成功：`{ok: true}`。**所有字段都不 live-apply**——只写入 config.toml，下次启动才生效。
/// 之前 token 改了会回 `new_token` 让前端换内存 token 继续操作，但这只是「前端伪装」：
/// state.token 是不可变 String，后端真正鉴权还是用旧 token，前端拿新 token fetch 会 401，
/// 而托盘菜单 URL 也不会更新。统一重启生效反而消除这种「前端 token 跟后端不同步」的 bug。
pub async fn set_config_handler(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
    Json(payload): Json<Config>,
) -> Response {
    use axum::http::StatusCode;
    if q.t != state.token {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    if let Err(e) = validate(&payload) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response();
    }
    if let Err(e) = save(&payload) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": format!("写入失败：{}", e)})),
        )
            .into_response();
    }
    Json(json!({"ok": true})).into_response()
}

/// `POST /api/theme?t=<token>` body=`{"theme": "dark"|"light"|"system"}` → live apply + 持久化。
///
/// 与其他配置字段不同：theme **走 live-apply**——前端切换按钮点击后立即生效，
/// 不要求重启。重启只用于「真正影响 server 启动」的字段（端口 / token / save_dir 等）。
///
/// 持久化策略：load 当前文件 → 覆盖 theme 字段 → save。这样不会丢失其他字段
/// （前端切 theme 时不需要把所有字段都 POST 回来）。
pub async fn set_theme_handler(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    use axum::http::StatusCode;
    if q.t != state.token {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let raw = payload
        .get("theme")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let theme = match normalize_theme(raw) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response();
        }
    };
    // live apply：改 state，所有后续 ws server_info 推送都会带新 theme
    *state.theme.lock().unwrap() = theme.clone();
    // 持久化：保留文件里其他字段，只覆盖 theme
    let mut cfg = load();
    cfg.theme = Some(theme.clone());
    if let Err(e) = save(&cfg) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": format!("写入失败：{}", e)})),
        )
            .into_response();
    }
    Json(json!({"ok": true, "theme": theme})).into_response()
}

/// `POST /api/mouse_sensitivity?t=<token>` body=`{"mouse_sensitivity": 1.5}` → live apply + 持久化。
///
/// 与 theme 同模式：改 state.mouse_sensitivity（ws dispatch MouseMove 时乘以该值），
/// 同时 load → 覆盖字段 → save 持久化，保留文件里其他字段。
///
/// 之所以 live-apply 而不是走标准 /api/config 重启路径：灵敏度是触控板手感偏好，
/// 调一次要求重启会让用户反复试值时极其烦躁。前端 range slider onchange 触发即可。
pub async fn set_mouse_sensitivity_handler(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    use axum::http::StatusCode;
    if q.t != state.token {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let raw = match payload.get("mouse_sensitivity").and_then(|v| v.as_f64()) {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "缺少 mouse_sensitivity 字段或不是数字"})),
            )
                .into_response();
        }
    };
    if !raw.is_finite() || raw < 0.1 || raw > 5.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "mouse_sensitivity 必须是 0.1-5.0 之间的有限数"})),
        )
            .into_response();
    }
    let sens = raw as f32;
    *state.mouse_sensitivity.lock().unwrap() = sens;
    let mut cfg = load();
    cfg.mouse_sensitivity = Some(sens);
    if let Err(e) = save(&cfg) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": format!("写入失败：{}", e)})),
        )
            .into_response();
    }
    Json(json!({"ok": true, "mouse_sensitivity": sens})).into_response()
}

#[derive(Deserialize)]
pub struct ListDirQuery {
    #[serde(flatten)]
    pub token: TokenQuery,
    pub path: Option<String>,
}

/// `GET /api/list_dir?t=<token>&path=<path>` → 目录浏览。
/// - `path` 为空：从用户主目录开始
/// - `path = "roots"`：返回顶层入口列表（Windows: 所有盘符；Unix: `/`）。
///   解决 Windows 上 home 目录在 C 盘、用户想选 D 盘时无路可走的问题
/// - 其他：列该路径下的目录
///
/// 不存在的路径返回 404，非目录返回 400。不做路径沙箱——token 已 gating，
/// 持有者本来就是机器主人。
pub async fn list_dir_handler(
    State(state): State<AppState>,
    Query(q): Query<ListDirQuery>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    use axum::http::StatusCode;
    if q.token.t != state.token {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let raw = q.path.unwrap_or_default();

    // 顶层入口视图：Windows 列盘符，Unix 直接给 `/`
    if raw == "roots" {
        let entries = list_roots();
        return Ok(Json(json!({
            "current": "roots",
            "parent": null,
            "entries": entries,
            "is_roots": true,
        })));
    }

    let start = if raw.trim().is_empty() {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else {
        PathBuf::from(&raw)
    };
    let canonical = start.canonicalize().map_err(|_| StatusCode::NOT_FOUND)?;
    if !canonical.is_dir() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let entries = match std::fs::read_dir(&canonical) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    return None; // 隐藏文件不显示，降低噪音
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if !is_dir {
                    return None; // 只列目录，文件无意义
                }
                Some(json!({"name": name, "is_dir": true}))
            })
            .collect::<Vec<_>>(),
        Err(_) => vec![],
    };
    Ok(Json(json!({
        "current": display_path(&canonical),
        "parent": canonical.parent().map(display_path),
        "entries": entries,
    })))
}

/// 把路径转成给前端显示用的字符串。
/// Windows 上 `Path::canonicalize` 会返回 `\\?\D:\foo` 这种 verbatim 前缀（启用
/// MAX_PATH 绕过），UI 显示成「此电脑 \ ?\ D:」很难看。我们的场景下路径不会超
/// MAX_PATH，剥掉前缀让面包屑干净。UNC 路径 `\\?\UNC\server\share` 还原成
/// `\\server\share`。
fn display_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", stripped)
    } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
        stripped.to_string()
    } else {
        s.to_string()
    }
}

/// 列出文件系统顶层入口。Windows: 所有盘符（C:\\ D:\\ ...）；Unix: 根目录 `/`。
#[cfg(target_os = "windows")]
fn list_roots() -> Vec<serde_json::Value> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDriveStringsW;

    let mut buf = [0u16; 256];
    let len = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) };
    if len == 0 {
        return vec![];
    }
    // API 返回的是 double-null-terminated 的 UTF-16 字符串序列，每段以 \0 结尾
    let s = String::from_utf16_lossy(&buf[..len as usize]);
    s.split('\0')
        .filter(|d| !d.is_empty())
        .map(|d| {
            // 去掉末尾的反斜杠作为显示名（"C:\\" → "C:"），路径保留末尾反斜杠
            let name = d.trim_end_matches('\\').to_string();
            json!({"name": name, "is_dir": true, "path": d})
        })
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn list_roots() -> Vec<serde_json::Value> {
    // Unix: 直接把 `/` 作为唯一根入口，前端进入后展示根目录子项
    vec![json!({"name": "/", "is_dir": true, "path": "/"})]
}

/// `POST /api/restart?t=<token>` → 让 qrctrl 重启以让新配置生效。
///
/// 流程：
/// 1. 通过 tray_proxy 给 tao event loop 发 RestartRequested
/// 2. tray 收到后 spawn `current_exe`（透传本进程 CLI 参数 + QRCTRL_RESTART_CHILD=1
///    环境变量让 probe_port 重试绑定），然后 notify server + ControlFlow::Exit
/// 3. tao 的 `event_loop.run()` 是 `-> !`，ControlFlow::Exit 后 Windows 直接
///    ExitProcess——所以 spawn 必须在 tray handler 里，main 中 run() 之后的代码不会执行
/// 4. 新进程 probe_port 重试 ~2 秒，等老进程释放端口后绑定成功
pub async fn restart_handler(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> Response {
    use axum::http::StatusCode;
    if q.t != state.token {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    // 给 tray 发 RestartRequested，tray handler 负责真正 spawn 新进程。
    // 同时让 server 优雅退出（释放 listener，给新进程让端口）。
    let _ = state.tray_proxy.send_event(crate::tray::UserEvent::RestartRequested);
    state.shutdown_notify.notify_waiters();
    Json(json!({"ok": true})).into_response()
}

/// `GET /api/local_ips?t=<token>` → LAN 接口列表（IP + 掩码 + CIDR + prefer 前缀）。
/// 前端用真实子网信息展示，比之前按 IP 字符串前两段硬切更准。
pub async fn local_ips_handler(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> Result<Json<Vec<serde_json::Value>>, axum::http::StatusCode> {
    if q.t != state.token {
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    }
    let interfaces = net::list_lan_interfaces()
        .into_iter()
        .map(|li| {
            json!({
                "ip": li.ip.to_string(),
                "netmask": li.netmask.to_string(),
                "prefix_len": li.prefix_len(),
                "network": li.network().to_string(),
                "cidr": format!("{}/{}", li.network(), li.prefix_len()),
                "prefer": li.prefer_prefix(),
                "name": li.name,
            })
        })
        .collect();
    Ok(Json(interfaces))
}

#[derive(Deserialize)]
pub struct CheckPortQuery {
    #[serde(flatten)]
    pub token: TokenQuery,
    pub addr: String,
    pub port: u16,
}

/// `GET /api/check_port?t=<token>&addr=<addr>&port=<port>` → bind+drop 预检。
/// 前端在保存前调，避免用户选了占用端口重启后崩溃。
///
/// **特例**：如果端口和 state.port（当前 qrctrl 自己监听的端口）一致，
/// 视为「可用」——我们自己在用，不算冲突。否则用户在配置页里 focus/blur
/// 端口字段（没改任何东西）就会提示「已被占用」。
pub async fn check_port_handler(
    State(state): State<AppState>,
    Query(q): Query<CheckPortQuery>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    if q.token.t != state.token {
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    }
    if q.port == state.port {
        return Ok(Json(json!({"free": true, "self": true})));
    }
    let bind = format!("{}:{}", q.addr, q.port);
    let free = std::net::TcpListener::bind(&bind).is_ok();
    Ok(Json(json!({"free": free})))
}
