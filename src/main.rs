mod clipboard;
mod file_transfer;
mod inject;
mod net;
mod qr;
mod state;
mod token;
mod ws;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{response::Html, routing::{get, post}, Router};
use clap::Parser;
use enigo::{Enigo, Settings};

use crate::state::AppState;

const INDEX_HTML: &str = include_str!("../static/index.html");

const DEFAULT_MAX_SIZE: u64 = 10 * 1024 * 1024 * 1024; // 10 GB
const REGISTRY_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(version, about = "qrctrl — 用手机扫码控制 PC")]
struct Cli {
    /// 监听地址
    #[arg(short, long, default_value = "0.0.0.0")]
    addr: String,

    /// 监听端口
    #[arg(short, long, default_value_t = 8080)]
    port: u16,

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

fn resolve_name(cli_name: Option<String>) -> String {
    if let Some(n) = cli_name {
        if !n.trim().is_empty() {
            return n;
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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let token = match &cli.token {
        Some(t) => {
            token::validate_token(t).unwrap_or_else(|e| panic!("--token 无效：{}", e));
            t.clone()
        }
        None => token::generate_token(),
    };
    let enigo = Enigo::new(&Settings::default()).expect("Enigo 初始化失败");
    let cb = clipboard::new_handle().expect("剪贴板初始化失败");
    let name = resolve_name(cli.name);
    let save_dir = resolve_save_dir(cli.save_dir);
    tokio::fs::create_dir_all(&save_dir)
        .await
        .unwrap_or_else(|e| panic!("创建 save_dir 失败 {}: {}", save_dir.display(), e));

    let registry = file_transfer::TransferRegistry::default();
    // 后台清理过期 transfer 项，避免内存堆积
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

    let bind = format!("{}:{}", cli.addr, cli.port);
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("端口绑定失败");

    // 收集局域网候选 IP，应用 --prefer-ip 过滤（若提供）
    let all_candidates = net::list_local_ipv4s();
    let candidates = match &cli.prefer_ip {
        Some(p) => net::filter_by_subnet(&all_candidates, p),
        None => all_candidates.clone(),
    };

    let url = match candidates.first() {
        Some(ip) => format!("http://{}:{}/?t={}", ip, cli.port, token),
        None => {
            eprintln!("[警告] 未检测到局域网 IPv4，回退到 localhost");
            format!("http://localhost:{}/?t={}", cli.port, token)
        }
    };

    println!("============================================");
    println!(" qrctrl 已启动 · 设备名：{}", name);
    println!("--------------------------------------------");
    println!(" 文件保存目录：{}", save_dir.display());
    println!(" 单文件上限：{} 字节", cli.max_size);
    println!("--------------------------------------------");
    println!(" 手机扫码连接（相机/微信扫一扫）：");
    println!();
    let _ = qr::render_qr_to_terminal(&url);
    println!();
    println!("--------------------------------------------");
    println!(" 或手动输入 URL：");
    println!("   {}", url);
    // 多网卡场景：把其他候选 IP 也列出来，让用户能挑手机所在网段那个扫
    if candidates.len() > 1 {
        println!("--------------------------------------------");
        println!(" 检测到多个网卡 IP，监听 0.0.0.0 全部可访问：");
        for ip in &candidates[1..] {
            println!("   http://{}:{}/?t={}", ip, cli.port, token);
        }
    }
    println!("============================================");
    println!("\n监听 {}，按 Ctrl+C 退出\n", bind);

    axum::serve(listener, app).await.unwrap();
}
