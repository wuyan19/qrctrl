//! 配置页的 HTTP API handlers（`/api/*`）。
//!
//! 与 `config.rs`（配置文件本体：load / save / validate）分离——配置页这个
//! 前端功能的服务端集合（读运行时状态、列目录、端口预检、重启）不属于
//! 「配置文件」的职责，混在一起会让 config.rs 无限膨胀。
//!
//! 所有 handler 通过 `Authed` extractor（`?t=<token>` 校验）统一鉴权。
//! `POST /api/config` 通过 axum `Json<T>` extractor 强制
//! `Content-Type: application/json`——浏览器跨站 POST 该 Content-Type 触发
//! CORS preflight，我们不开 CORS，等于免费 CSRF 防御。

use std::path::PathBuf;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::config;
use crate::net;
use crate::state::AppState;

/// `GET /api/config?t=<token>` → 当前生效配置 JSON。
///
/// 注意：`port` 回显的是探测后的**实际监听端口**（如 8080 被占自动改用 8081 时
/// 回显 8081）。用户在配置页不动端口直接保存，这个值会固化进 config.toml——
/// 已知取舍：回显「当前生效值」比回显「文件里的值」更符合直觉。
pub async fn get_config_handler(
    _: crate::state::Authed,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let c = &state.core;
    Ok(Json(json!({
        "addr": c.addr,
        "port": c.port,
        "name": c.name,
        "save_dir": c.save_dir.to_string_lossy(),
        "max_size": c.max_size,
        "token": c.token,
        "prefer_ip": c.prefer_ip,
        "theme": *c.theme.lock(),
        "mouse_sensitivity": *c.mouse_sensitivity.lock(),
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
    _: crate::state::Authed,
    State(_state): State<AppState>,
    Json(payload): Json<config::Config>,
) -> Response {
    use axum::http::StatusCode;
    if let Err(e) = config::validate(&payload) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response();
    }
    if let Err(e) = config::save(&payload) {
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
    _: crate::state::Authed,
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    use axum::http::StatusCode;
    let raw = payload
        .get("theme")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let theme = match config::normalize_theme(raw) {
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
    *state.core.theme.lock() = theme.clone();
    // 持久化：保留文件里其他字段，只覆盖 theme
    let mut cfg = config::load();
    cfg.theme = Some(theme.clone());
    if let Err(e) = config::save(&cfg) {
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
/// 与 theme 同模式：改 state.core.mouse_sensitivity（ws dispatch MouseMove 时乘以该值），
/// 同时 load → 覆盖字段 → save 持久化，保留文件里其他字段。
///
/// 之所以 live-apply 而不是走标准 /api/config 重启路径：灵敏度是触控板手感偏好，
/// 调一次要求重启会让用户反复试值时极其烦躁。前端 range slider onchange 触发即可。
pub async fn set_mouse_sensitivity_handler(
    _: crate::state::Authed,
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    use axum::http::StatusCode;
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
    *state.core.mouse_sensitivity.lock() = sens;
    let mut cfg = config::load();
    cfg.mouse_sensitivity = Some(sens);
    if let Err(e) = config::save(&cfg) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": format!("写入失败：{}", e)})),
        )
            .into_response();
    }
    Json(json!({"ok": true, "mouse_sensitivity": sens})).into_response()
}

/// `GET /api/list_dir?t=<token>&path=<path>` 的查询参数。
/// token 校验由 `Authed` 完成，这里只剩业务字段。
#[derive(Deserialize)]
pub struct ListDirQuery {
    pub path: Option<String>,
}

/// `GET /api/list_dir?t=<token>&path=<path>` → 目录浏览。
/// - `path` 为空：从用户主目录开始
/// - `path = "roots"`：返回顶层入口列表（Windows: 所有盘符；Unix: `/`）。
///   解决 Windows 上 home 目录在 C 盘、用户想选 D 盘时无路可走的问题
/// - 其他：列该路径下的目录
///
/// 不存在的路径返回 404，非目录返回 400。不做路径沙箱——token 已 gating，
/// 持有者本来就是机器主人。canonicalize / read_dir 是阻塞 syscall
///（网络盘/大目录可达上百毫秒），丢进 spawn_blocking 不卡 tokio worker。
pub async fn list_dir_handler(
    _: crate::state::Authed,
    State(_state): State<AppState>,
    Query(q): Query<ListDirQuery>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    use axum::http::StatusCode;
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
    let result = tokio::task::spawn_blocking(move || list_dir_sync(start)).await;
    match result {
        Ok(Ok((canonical, entries))) => Ok(Json(json!({
            "current": display_path(&canonical),
            "parent": canonical.parent().map(display_path),
            "entries": entries,
        }))),
        Ok(Err(code)) => Err(code),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// list_dir 的阻塞部分：canonicalize + 判目录 + read_dir 收集子目录。
/// 隐藏文件（`.` 开头）和非目录条目过滤掉——目录选择模态只要目录。
fn list_dir_sync(
    start: PathBuf,
) -> Result<(PathBuf, Vec<serde_json::Value>), axum::http::StatusCode> {
    use axum::http::StatusCode;
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
    Ok((canonical, entries))
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
    _: crate::state::Authed,
    State(state): State<AppState>,
) -> Response {
    // 给 tray 发 RestartRequested，tray handler 负责真正 spawn 新进程。
    // 同时让 server 优雅退出（释放 listener，给新进程让端口）。
    let _ = state
        .tray_proxy
        .send_event(crate::tray::UserEvent::RestartRequested);
    state.shutdown_notify.notify_waiters();
    Json(json!({"ok": true})).into_response()
}

/// `GET /api/local_ips?t=<token>` → LAN 接口列表（IP + 掩码 + CIDR + prefer 前缀）。
/// 前端用真实子网信息展示，比之前按 IP 字符串前两段硬切更准。
pub async fn local_ips_handler(
    _: crate::state::Authed,
    State(_state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, axum::http::StatusCode> {
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

/// `GET /api/check_port?t=<token>&addr=<addr>&port=<port>` 的查询参数。
/// token 校验由 `Authed` 完成。
#[derive(Deserialize)]
pub struct CheckPortQuery {
    pub addr: String,
    pub port: u16,
}

/// `GET /api/check_port?t=<token>&addr=<addr>&port=<port>` → bind+drop 预检。
/// 前端在保存前调，避免用户选了占用端口重启后崩溃。
///
/// **特例**：如果端口和 state.core.port（当前 qrctrl 自己监听的端口）一致，
/// 视为「可用」——我们自己在用，不算冲突。否则用户在配置页里 focus/blur
/// 端口字段（没改任何东西）就会提示「已被占用」。
///
/// bind 是阻塞 syscall，丢进 spawn_blocking 不卡 tokio worker。
pub async fn check_port_handler(
    _: crate::state::Authed,
    State(state): State<AppState>,
    Query(q): Query<CheckPortQuery>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    if q.port == state.core.port {
        return Ok(Json(json!({"free": true, "self": true})));
    }
    let bind = format!("{}:{}", q.addr, q.port);
    // join 失败（线程池 panic）当作「不可用」返回，宁可保守也不 500——
    // 这只是个预检接口，前端会显示占用让用户自己再试。
    let free = tokio::task::spawn_blocking(move || std::net::TcpListener::bind(&bind).is_ok())
        .await
        .unwrap_or(false);
    Ok(Json(json!({"free": free})))
}
