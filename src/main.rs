// release 模式用 windows GUI subsystem：双击不弹 cmd 黑窗，关终端不影响程序。
// debug 模式保留 console，方便开发时直接看 println!/panic 信息。
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod clipboard;
mod config;
mod file_transfer;
mod inject;
mod net;
mod qr;
mod state;
mod token;
mod tray;
mod ws;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{extract::State, response::Html, routing::{get, post}, Router};
use clap::Parser;
use enigo::{Enigo, Settings};
use tokio::sync::Notify;

use crate::state::AppState;
use crate::tray::TrayState;

const INDEX_HTML: &str = include_str!("../static/index.html");

const DEFAULT_MAX_SIZE: u64 = 10 * 1024 * 1024 * 1024; // 10 GB
const REGISTRY_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(version, about = "QR Control")]
struct Cli {
    /// 监听地址
    #[arg(short, long)]
    addr: Option<String>,

    /// 监听端口。不传时从 8080 起按 +1 递增找一个可用端口（最多到 8129），
    /// 适配双击启动但 8080 被占用的场景；显式传 `--port` 时只试这个端口，
    /// 被占用就 panic（尊重用户明确选择）。
    #[arg(short, long)]
    port: Option<u16>,

    /// 设备名称（用于手机端状态栏显示，区分多台被控设备）
    #[arg(short, long)]
    name: Option<String>,

    /// 文件保存目录（手机上传的文件落到这里，默认 <系统下载目录>/qrctrl/）
    #[arg(long)]
    save_dir: Option<PathBuf>,

    /// 单个文件大小上限（字节，默认 10 GB）
    #[arg(long)]
    max_size: Option<u64>,

    /// 固定 token（用于重启后保持扫码 URL 不变，手机端刷新页面即可重连）
    /// 默认每次启动随机生成。提供时必须是 4-64 位 ASCII 字母数字。
    #[arg(long)]
    token: Option<String>,

    /// 偏好的 IP 子网前缀（多网卡时用来选 QR 码用的 IP）
    /// 例如 `--prefer-ip 192.168.20` 会优先把 192.168.20.x 的 IP 放进 QR 码。
    /// 不匹配时回退到默认候选。
    #[arg(long)]
    prefer_ip: Option<String>,
}

/// 在 windows_subsystem = "windows" 模式下，stdout/stderr 默认无效。
/// 如果是从 cmd/PowerShell 启动的（有父 console），attach 上并把 std handle
/// 重绑过去——这样 banner / --help / --version / panic 都能正常输出。
/// 双击启动时 AttachConsole 失败（无父 console），静默跳过，程序继续。
///
/// 返回 true 表示当前进程**有可用的 stdout**（已有 console 或 attach 成功）；
/// 返回 false 表示无 console（双击启动），调用方据此决定是否自动弹 GUI 二维码窗口。
#[cfg(target_os = "windows")]
fn attach_parent_console() -> bool {
    use std::ptr;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileA, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    unsafe {
        // 已有 stdout（console subsystem 直接启动，或 debug 模式）→ 不需要 attach
        let existing = GetStdHandle(STD_OUTPUT_HANDLE);
        if !existing.is_null() && existing != INVALID_HANDLE_VALUE {
            return true;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return false; // 双击启动，无父 console
        }
        let name = b"CONOUT$\0";
        let out = CreateFileA(
            name.as_ptr(),
            (GENERIC_READ | GENERIC_WRITE) as u32,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        );
        if out.is_null() || out == INVALID_HANDLE_VALUE {
            return false;
        }
        SetStdHandle(STD_OUTPUT_HANDLE, out);
        SetStdHandle(STD_ERROR_HANDLE, out);
        true
    }
}

fn resolve_name(cli_name: &Option<String>) -> String {
    if let Some(n) = cli_name {
        if !n.trim().is_empty() {
            return n.clone();
        }
    }
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "qrctrl".to_string())
}

/// 解析 save_dir：用户/配置文件显式提供 → 原样使用；都没给 → 默认 `<下载目录>/qrctrl`。
///
/// 早期版本无条件在末尾 join("qrctrl")，导致每次「保存配置 → 重启」循环都自动多一层
/// （D:\Share → D:\Share\qrctrl → D:\Share\qrctrl\qrctrl → …）。改成显式提供即原样使用，
/// 只在没有任何输入时才 fallback 到下载目录 + qrctrl 子目录，行为幂等。
fn resolve_save_dir(cli_save_dir: Option<PathBuf>) -> PathBuf {
    if let Some(p) = cli_save_dir.filter(|p| !p.as_os_str().is_empty()) {
        return p;
    }
    dirs::download_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qrctrl")
}

/// 探测可用端口。bind 成功后立即 drop listener，只返回端口号。
/// - `requested = Some(p)`：用户显式指定，只试这个端口，失败就 panic。
/// - `requested = None`：双击启动场景，从 8080 起按 +1 递增试，最多 50 次。
///   全失败才 panic。
///
/// 真正的 listener 在 server 线程内用 `tokio::net::TcpListener::bind` 重新绑定——
/// 因为 std::net::TcpListener 跨线程通过 `from_std` 移交给 tokio 后，在 Windows 上
/// IOCP 注册路径无法正常 accept（端口 LISTENING 但所有连接 timeout）。macOS / Linux
/// 无此问题，但代码统一走 tokio bind 路径避免平台分歧。
/// TOCTOU 风险（探测后被抢）窗口是毫秒级，可接受。
///
/// 特例：环境变量 `QRCTRL_RESTART_CHILD=1` 表示本进程是配置页「立即重启」spawn 出来的
/// 子进程——老进程刚退出但 listener 可能还没完全释放，给最多 ~2 秒重试窗口避免新进程
/// bind 失败崩溃。其他场景（双击启动 / CLI 启动）保持 fail-fast。
fn probe_port(addr: &str, requested: Option<u16>) -> u16 {
    const PROBE_RANGE: u16 = 50;
    const DEFAULT_PORT: u16 = 8080;
    const RESTART_RETRY_MS: u64 = 100;
    const RESTART_RETRY_MAX: u32 = 20; // 20 × 100ms = 2 秒

    let candidates: Vec<u16> = match requested {
        Some(p) => vec![p],
        None => (DEFAULT_PORT..DEFAULT_PORT + PROBE_RANGE).collect(),
    };
    let is_restart_child = std::env::var("QRCTRL_RESTART_CHILD").is_ok();
    let attempts = if is_restart_child { RESTART_RETRY_MAX } else { 1 };

    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..attempts {
        for &p in &candidates {
            let bind_addr = format!("{}:{}", addr, p);
            match std::net::TcpListener::bind(&bind_addr) {
                Ok(_) => return p,
                Err(e) => last_err = Some(e),
            }
        }
        if attempt + 1 < attempts {
            std::thread::sleep(std::time::Duration::from_millis(RESTART_RETRY_MS));
        }
    }
    match requested {
        Some(p) => panic!(
            "端口 {}:{} 绑定失败（被占用或权限不足）：{}",
            addr,
            p,
            last_err.unwrap()
        ),
        None => panic!(
            "端口范围 {}:{}-{} 全部被占用，请用 --port 指定其他端口",
            addr,
            candidates.first().unwrap(),
            candidates.last().unwrap()
        ),
    }
}

fn main() {
    // has_console = 是否有可用的 stdout。双击启动时为 false（自动弹 GUI 二维码窗口），
    // PowerShell/cmd/terminal 启动时为 true（banner 在终端显示，不需要 GUI 弹窗）。
    #[cfg(target_os = "windows")]
    let has_console = attach_parent_console();
    #[cfg(not(target_os = "windows"))]
    let has_console = std::io::IsTerminal::is_terminal(&std::io::stdout());

    let cli = Cli::parse();

    // 三层配置合并：built-in default → config.toml → CLI args（CLI 永远赢）。
    // config.toml 损坏时 load() 内部已 graceful 降级（rename 备份 + 返回 default），不 panic。
    let file_cfg = config::load();

    let token = match &cli.token {
        Some(t) => {
            token::validate_token(t).unwrap_or_else(|e| panic!("--token 无效：{}", e));
            t.clone()
        }
        None => match &file_cfg.token {
            Some(t) => {
                token::validate_token(t).unwrap_or_else(|e| panic!("config.toml token 无效：{}", e));
                t.clone()
            }
            None => token::generate_token(),
        },
    };
    let name = resolve_name(&cli.name.or(file_cfg.name.clone()));

    // CLI 优先 → 配置文件 → built-in default（下载目录下的 qrctrl 子目录）
    let save_dir = resolve_save_dir(cli.save_dir.clone().or(file_cfg.save_dir.clone()));
    std::fs::create_dir_all(&save_dir)
        .unwrap_or_else(|e| panic!("创建 save_dir 失败 {}: {}", save_dir.display(), e));

    let addr = cli.addr.clone().or(file_cfg.addr.clone()).unwrap_or_else(|| "0.0.0.0".to_string());
    let max_size = cli.max_size.or(file_cfg.max_size).unwrap_or(DEFAULT_MAX_SIZE);
    let prefer_ip = cli.prefer_ip.clone().or(file_cfg.prefer_ip.clone());
    // 主题：config.toml 显式给出 → 用之；都没有 → "system"（前端按 prefers-color-scheme 决定）。
    // CLI 不暴露 theme 参数——这是 UI 偏好而非启动配置，CLI 化没意义。
    let theme = file_cfg.theme.clone().unwrap_or_else(|| "system".to_string());
    // 触控板灵敏度：同 theme 是 UI 偏好，CLI 不暴露。config.toml 给值即用，否则默认 1.0。
    let mouse_sensitivity = file_cfg.mouse_sensitivity.unwrap_or(1.0);

    // 端口探测（用户没传 --port 时从 8080 起递增找可用端口）。
    // 实际 listener 在 server 线程内由 tokio 重新 bind，见上方 probe_port 注释。
    let port = probe_port(&addr, cli.port.or(file_cfg.port));
    if cli.port.is_none() && file_cfg.port.is_none() && port != 8080 {
        // 双击启动时无 console，这行只在 CLI 启动可见；QR 码本身已含正确端口
        println!("[qrctrl] 默认端口 8080 被占用，已自动改用 {}", port);
    }

    // 收集局域网候选 IP，应用 --prefer-ip 过滤（若提供）
    let all_candidates = net::list_local_ipv4s();
    let candidates = match &prefer_ip {
        Some(p) => net::filter_by_subnet(&all_candidates, p),
        None => all_candidates.clone(),
    };
    let url = match candidates.first() {
        Some(ip) => format!("http://{}:{}/?t={}", ip, port, token),
        None => {
            eprintln!("[警告] 未检测到局域网 IPv4，回退到 localhost");
            format!("http://localhost:{}/?t={}", port, token)
        }
    };

    // banner 先打印（用 & 借，不 move）
    print_banner(&name, &url, &candidates, &addr, max_size, port, &save_dir);

    // shutdown 通知：tray 退出菜单 / 配置页「立即重启」都通过它让 server 优雅退出
    let shutdown_notify = Arc::new(Notify::new());

    // event_loop 必须在主线程创建（macOS NSApplication 主线程约束），
    // 但要先把它的 proxy 派发到 server 线程，restart_handler 才能给 tray 发退出信号。
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut event_loop = tao::event_loop::EventLoopBuilder::<tray::UserEvent>::with_user_event().build();
    let tray_proxy = event_loop.create_proxy();

    // server 跑在子线程：tokio runtime + axum。
    // 主线程必须留给 tao event loop（macOS NSApplication 主线程约束）。
    let server_shutdown = shutdown_notify.clone();
    let server_name = name.clone();
    let tray_save_dir = save_dir.clone();
    let server_handle = std::thread::Builder::new()
        .name("qrctrl-server".to_string())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime 初始化失败");
            rt.block_on(async_main(
                token,
                server_name,
                addr,
                port,
                prefer_ip,
                max_size,
                theme,
                mouse_sensitivity,
                server_shutdown,
                tray_proxy,
                save_dir,
            ));
        })
        .expect("server 线程启动失败");

    // 主线程跑 tray 事件循环（阻塞，直到用户选退出 / restart 触发）
    let tray_state = TrayState {
        device_name: name,
        url,
        save_dir: tray_save_dir,
        auto_show_qr: !has_console,
    };
    tray::run_tray_event_loop(tray_state, shutdown_notify.clone(), event_loop);

    // 注意：tao 的 event_loop.run() 是 `-> !`，ControlFlow::Exit 后 Windows
    // 直接 ExitProcess，macOS/Linux 也类似，run() 之后的代码不会执行。
    // 所以「立即重启」的 spawn 新进程逻辑放在 tray::RestartRequested 分支里，
    // 这里只保留 quit 路径的 graceful shutdown 退出兜底（实际几乎跑不到）。
    println!("[qrctrl] 正在退出...");
    if server_handle.join().is_err() {
        eprintln!("[qrctrl] server 线程 panic，强制退出");
        std::process::exit(1);
    }
}

/// `GET /` → index.html，首屏注入当前主题。
///
/// 主题占位符 `data-theme="__THEME__"` 在 HTML 模板里。这里读 `state.theme`（可能被
/// `set_theme_handler` 在运行时改过）替换占位符。inline `<script>` 会同步把 `"system"`
/// 解析成 dark/light 应用到 `<html>`，避免 CSS 应用后的 FOUC。
async fn index_handler(State(state): State<AppState>) -> Html<String> {
    let theme = state.theme.lock().unwrap().clone();
    let name = escape_html(&state.name);
    let html = INDEX_HTML
        .replace(
            "data-theme=\"__THEME__\"",
            &format!("data-theme=\"{}\"", theme),
        )
        .replace(
            "<title>__DEVICE_NAME__</title>",
            &format!("<title>{}</title>", name),
        );
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

async fn async_main(
    token: String,
    name: String,
    addr: String,
    port: u16,
    prefer_ip: Option<String>,
    max_size: u64,
    theme: String,
    mouse_sensitivity: f32,
    shutdown_notify: Arc<Notify>,
    tray_proxy: tao::event_loop::EventLoopProxy<tray::UserEvent>,
    save_dir: PathBuf,
) {
    // listener 在 server 线程内由 tokio 直接 bind（不走 main → from_std 路径，
    // 因为 Windows IOCP 下跨线程 from_std 不能正常 accept）。main 阶段已用
    // probe_port 同步探测过，这里重新 bind 仅在 TOCTOU 极端情况下才会失败。
    // 必须在构造 AppState 前 bind：addr 后续会 move 进 state。
    let bind_addr = format!("{}:{}", addr, port);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|e| panic!("端口 {} 绑定失败：{}", bind_addr, e));

    let enigo = Enigo::new(&Settings::default()).expect("Enigo 初始化失败");
    let cb = clipboard::new_handle().expect("剪贴板初始化失败");
    // save_dir 由 main 同步创建，这里无需重复

    let registry = file_transfer::TransferRegistry::default();
    {
        let reg = registry.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REGISTRY_CLEANUP_INTERVAL).await;
                reg.cleanup_expired();
            }
        });
    }

    let state = AppState {
        token: token.clone(),
        name: name.clone(),
        addr,
        port,
        prefer_ip,
        enigo: Arc::new(Mutex::new(enigo)),
        clipboard: cb,
        save_dir: save_dir.clone(),
        max_size,
        registry,
        shutdown_notify: shutdown_notify.clone(),
        tray_proxy,
        theme: Arc::new(Mutex::new(theme)),
        mouse_sensitivity: Arc::new(Mutex::new(mouse_sensitivity)),
    };

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws::ws_handler))
        .route("/upload/{id}", post(file_transfer::upload_handler))
        .route("/download/{id}", get(file_transfer::download_handler))
        .route("/config", get(config::config_page_handler))
        .route("/api/config", get(config::get_config_handler).post(config::set_config_handler))
        .route("/api/theme", post(config::set_theme_handler))
        .route("/api/mouse_sensitivity", post(config::set_mouse_sensitivity_handler))
        .route("/api/list_dir", get(config::list_dir_handler))
        .route("/api/local_ips", get(config::local_ips_handler))
        .route("/api/check_port", get(config::check_port_handler))
        .route("/api/restart", post(config::restart_handler))
        .with_state(state);

    // graceful shutdown：tray 退出菜单触发 notify，server 收到信号后优雅关闭
    // restart_handler 也通过同一个 notify 让 server 跟着退出
    let shutdown_signal = async move {
        shutdown_notify.notified().await;
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .unwrap();
}

fn print_banner(
    name: &str,
    url: &str,
    candidates: &[Ipv4Addr],
    addr: &str,
    max_size: u64,
    port: u16,
    save_dir: &std::path::Path,
) {
    let token_in_url = url.rsplit_once("t=").map(|(_, t)| t).unwrap_or("");
    println!("============================================");
    println!(" qrctrl 已启动 · 设备名：{}", name);
    println!("--------------------------------------------");
    println!(" 文件保存目录：{}", save_dir.display());
    println!(" 单文件上限：{} 字节", max_size);
    println!("--------------------------------------------");
    println!(" 手机扫码连接（相机/微信扫一扫）：");
    println!();
    let _ = qr::render_qr_to_terminal(url);
    println!();
    println!("--------------------------------------------");
    println!(" 或手动输入 URL：");
    println!("   {}", url);
    if candidates.len() > 1 {
        println!("--------------------------------------------");
        println!(" 检测到多个网卡 IP，监听 0.0.0.0 全部可访问：");
        for ip in &candidates[1..] {
            println!("   http://{}:{}/?t={}", ip, port, token_in_url);
        }
    }
    println!("============================================");
    println!("\n监听 {}:{}，托盘图标常驻，菜单选择退出\n", addr, port);
}
