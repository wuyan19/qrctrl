// release 模式用 windows GUI subsystem：双击不弹 cmd 黑窗，关终端不影响程序。
// debug 模式保留 console，方便开发时直接看 println!/panic 信息。
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod clipboard;
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

use axum::{response::Html, routing::{get, post}, Router};
use clap::Parser;
use enigo::{Enigo, Settings};
use tokio::sync::Notify;

use crate::state::AppState;
use crate::tray::TrayState;

const INDEX_HTML: &str = include_str!("../static/index.html");

const DEFAULT_MAX_SIZE: u64 = 10 * 1024 * 1024 * 1024; // 10 GB
const REGISTRY_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(version, about = "qrctrl — 用手机扫码控制 PC")]
struct Cli {
    /// 监听地址
    #[arg(short, long, default_value = "0.0.0.0")]
    addr: String,

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
    #[arg(long, default_value_t = DEFAULT_MAX_SIZE)]
    max_size: u64,

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

/// 解析 save_dir：cli 优先 → 系统下载目录 → 当前目录；末尾加 qrctrl 子目录。
fn resolve_save_dir(cli_save_dir: Option<PathBuf>) -> PathBuf {
    let base = cli_save_dir
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(dirs::download_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("qrctrl")
}

/// 探测并绑定端口。
/// - `requested = Some(p)`：用户显式指定，只试这个端口，失败就 panic。
/// - `requested = None`：双击启动场景，从 8080 起按 +1 递增试，最多 50 次。
///   全失败才 panic。
///
/// 返回 (实际绑定的端口, std::net::TcpListener)。listener 在 main 创建后一路 move
/// 到 async_main 用 `tokio::net::TcpListener::from_std` 转换，**不重新 bind**——
/// 避免「探测时可用、绑定时被抢」的 TOCTOU 竞态。
fn probe_and_bind_port(addr: &str, requested: Option<u16>) -> (u16, std::net::TcpListener) {
    const PROBE_RANGE: u16 = 50;
    const DEFAULT_PORT: u16 = 8080;
    let candidates: Vec<u16> = match requested {
        Some(p) => vec![p],
        None => (DEFAULT_PORT..DEFAULT_PORT + PROBE_RANGE).collect(),
    };
    let mut last_err: Option<std::io::Error> = None;
    for &p in &candidates {
        let bind_addr = format!("{}:{}", addr, p);
        match std::net::TcpListener::bind(&bind_addr) {
            Ok(listener) => return (p, listener),
            Err(e) => last_err = Some(e),
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

    let token = match &cli.token {
        Some(t) => {
            token::validate_token(t).unwrap_or_else(|e| panic!("--token 无效：{}", e));
            t.clone()
        }
        None => token::generate_token(),
    };
    let name = resolve_name(&cli.name);

    // save_dir 在 main 同步阶段创建：托盘菜单「打开文件保存目录」可能在 server
    // 线程完成 async_main 之前就被点，那时必须保证目录已存在。
    let save_dir = resolve_save_dir(cli.save_dir.clone());
    std::fs::create_dir_all(&save_dir)
        .unwrap_or_else(|e| panic!("创建 save_dir 失败 {}: {}", save_dir.display(), e));

    // 端口探测 + 绑定（用户没传 --port 时从 8080 起递增找可用端口）。
    // listener 一路 move 到 async_main，from_std 转换，不重新 bind。
    let (port, std_listener) = probe_and_bind_port(&cli.addr, cli.port);
    if cli.port.is_none() && port != 8080 {
        // 双击启动时无 console，这行只在 CLI 启动可见；QR 码本身已含正确端口
        println!("[qrctrl] 默认端口 8080 被占用，已自动改用 {}", port);
    }

    // 收集局域网候选 IP，应用 --prefer-ip 过滤（若提供）
    let all_candidates = net::list_local_ipv4s();
    let candidates = match &cli.prefer_ip {
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
    print_banner(&name, &url, &candidates, &cli, port, &save_dir);

    // shutdown 通知：tray 退出菜单触发 → server graceful shutdown
    let shutdown_notify = Arc::new(Notify::new());

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
                cli,
                token,
                server_name,
                server_shutdown,
                save_dir,
                std_listener,
            ));
        })
        .expect("server 线程启动失败");

    // 主线程跑 tray 事件循环（阻塞，直到用户选退出）
    let tray_state = TrayState {
        device_name: name,
        url,
        save_dir: tray_save_dir,
        auto_show_qr: !has_console,
    };
    tray::run_tray_event_loop(tray_state, shutdown_notify.clone());

    // tray 退出后等 server 关闭（graceful shutdown）
    println!("[qrctrl] 正在退出...");
    if server_handle.join().is_err() {
        eprintln!("[qrctrl] server 线程 panic，强制退出");
        std::process::exit(1);
    }
}

async fn async_main(
    cli: Cli,
    token: String,
    name: String,
    shutdown_notify: Arc<Notify>,
    save_dir: PathBuf,
    listener: std::net::TcpListener,
) {
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
        enigo: Arc::new(Mutex::new(enigo)),
        clipboard: cb,
        save_dir: save_dir.clone(),
        max_size: cli.max_size,
        registry,
    };

    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/ws", get(ws::ws_handler))
        .route("/upload/{id}", post(file_transfer::upload_handler))
        .route("/download/{id}", get(file_transfer::download_handler))
        .with_state(state);

    // listener 在 main 里通过 probe_and_bind_port 同步绑定，这里转 tokio（自动设非阻塞）。
    // 不重新 bind 是为了避免「探测可用、绑定时被抢」的 TOCTOU 竞态。
    let listener = tokio::net::TcpListener::from_std(listener)
        .expect("std TcpListener 转 tokio 失败");

    // graceful shutdown：tray 退出菜单触发 notify，server 收到信号后优雅关闭
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
    cli: &Cli,
    port: u16,
    save_dir: &std::path::Path,
) {
    let token_in_url = url.rsplit_once("t=").map(|(_, t)| t).unwrap_or("");
    println!("============================================");
    println!(" qrctrl 已启动 · 设备名：{}", name);
    println!("--------------------------------------------");
    println!(" 文件保存目录：{}", save_dir.display());
    println!(" 单文件上限：{} 字节", cli.max_size);
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
    println!("\n监听 {}:{}，托盘图标常驻，菜单选择退出\n", cli.addr, port);
}
