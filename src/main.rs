mod inject;
mod net;
mod qr;
mod state;
mod token;
mod ws;

use std::sync::{Arc, Mutex};

use axum::{response::Html, routing::get, Router};
use enigo::{Enigo, Settings};

use crate::state::AppState;

const INDEX_HTML: &str = include_str!("../static/index.html");

#[tokio::main]
async fn main() {
    let token = token::generate_token();
    let enigo = Enigo::new(&Settings::default()).expect("Enigo 初始化失败");
    let state = AppState {
        token: token.clone(),
        enigo: Arc::new(Mutex::new(enigo)),
    };

    let app = Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/ws", get(ws::ws_handler))
        .with_state(state);

    let port: u16 = 8080;
    let bind = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("端口绑定失败");

    let url = match net::get_local_ipv4() {
        Some(ip) => format!("http://{}:{}/?t={}", ip, port, token),
        None => {
            eprintln!("[警告] 未检测到局域网 IPv4，回退到 localhost");
            format!("http://localhost:{}/?t={}", port, token)
        }
    };

    println!("============================================");
    println!(" qrctrl 已启动");
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
