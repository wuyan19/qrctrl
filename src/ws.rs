use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;

use crate::inject;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct WsQuery {
    pub t: String,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<WsQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    if q.t != state.token {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state)))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    println!("[ws] 客户端已连接");
    while let Some(msg) = socket.recv().await {
        match msg {
            Ok(Message::Text(text)) => {
                let s = text.to_string();
                let enigo = state.enigo.clone();
                // enigo 阻塞调用，丢到 blocking 线程池，避免卡 executor
                let result = tokio::task::spawn_blocking(move || {
                    inject::inject_text(&enigo, &s)
                }).await;

                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => eprintln!("[ws] 注入失败: {}", e),
                    Err(e) => eprintln!("[ws] spawn_blocking join 失败: {}", e),
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    println!("[ws] 客户端断开");
}
