mod clipboard;
mod inject;
mod net;
mod qr;
mod state;
mod token;
mod ws;

use std::sync::{Arc, Mutex};

use axum::{response::Html, routing::get, Router};
use clap::Parser;
use enigo::{Enigo, Settings};

use crate::state::AppState;

const INDEX_HTML: &str = include_str!("../static/index.html");

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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let token = token::generate_token();
    let enigo = Enigo::new(&Settings::default()).expect("Enigo 初始化失败");
    let cb = clipboard::new_handle().expect("剪贴板初始化失败");
    let name = resolve_name(cli.name);
    let state = AppState {
        token: token.clone(),
        name: name.clone(),
        enigo: Arc::new(Mutex::new(enigo)),
        clipboard: cb,
    };

    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/ws", get(ws::ws_handler))
        .with_state(state);

    let bind = format!("{}:{}", cli.addr, cli.port);
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("端口绑定失败");

    let url = match net::get_local_ipv4() {
        Some(ip) => format!("http://{}:{}/?t={}", ip, cli.port, token),
        None => {
            eprintln!("[警告] 未检测到局域网 IPv4，回退到 localhost");
            format!("http://localhost:{}/?t={}", cli.port, token)
        }
    };

    println!("============================================");
    println!(" qrctrl 已启动 · 设备名：{}", name);
    println!("--------------------------------------------");
    println!(" 手机扫码连接（相机/微信扫一扫）：");
    println!();
    let _ = qr::render_qr_to_terminal(&url);
    println!();
    println!("--------------------------------------------");
    println!(" 或手动输入 URL：");
    println!("   {}", url);
    println!("============================================");
    println!("\n监听 {}，按 Ctrl+C 退出\n", bind);

    axum::serve(listener, app).await.unwrap();
}
