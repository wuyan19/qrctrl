use std::sync::{Arc, Mutex};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::StatusCode,
    response::Response,
};
use enigo::Enigo;
use serde::Deserialize;

use crate::clipboard;
use crate::file_transfer::{self, UploadMeta};
use crate::inject;
use crate::state::{AppState, TokenQuery};

const MAX_TEXT_BYTES: usize = 100 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum MouseButton {
    Left,
    Right,
    Middle,
}

impl From<MouseButton> for enigo::Button {
    fn from(b: MouseButton) -> Self {
        match b {
            MouseButton::Left => enigo::Button::Left,
            MouseButton::Right => enigo::Button::Right,
            MouseButton::Middle => enigo::Button::Middle,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    Text { value: String },
    GetClipboardText,
    GetClipboardImage,
    SetClipboardImage { data: String },
    UploadStart { name: String, size: u64, mime: String },
    GetFile,
    Enter,
    Tab,
    Backspace,
    Copy,
    Paste,
    MouseMove { dx: i32, dy: i32 },
    MouseClick { button: MouseButton },
    MousePress { button: MouseButton },
    MouseRelease { button: MouseButton },
    MouseScroll { dy: i32 },
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<TokenQuery>,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    if q.t != state.token {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state)))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    println!("[ws] 客户端已连接");
    // 升级后立刻推送设备名，前端用于状态栏显示
    let info = server_info_json(&state.name);
    if socket.send(Message::Text(info.into())).await.is_err() {
        println!("[ws] 发送 server_info 失败，断开");
        return;
    }
    while let Some(msg) = socket.recv().await {
        match msg {
            Ok(Message::Text(text)) => {
                let resp = dispatch(&state, text.as_str()).await;
                if socket.send(Message::Text(resp.into())).await.is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    println!("[ws] 客户端断开");
}

async fn dispatch(state: &AppState, raw: &str) -> String {
    let cmd: Command = match serde_json::from_str(raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ws] 指令解析失败: {}", e);
            return error_json("decode_failed");
        }
    };
    match cmd {
        Command::Text { mut value } => {
            truncate_in_place(&mut value, MAX_TEXT_BYTES);
            let enigo = state.enigo.clone();
            let result = tokio::task::spawn_blocking(move || inject::inject_text(&enigo, &value))
                .await;
            match result {
                Ok(Ok(())) => ok_json(),
                Ok(Err(e)) => {
                    eprintln!("[ws] 注入失败: {}", e);
                    error_json("inject_failed")
                }
                Err(e) => {
                    eprintln!("[ws] spawn_blocking join 失败: {}", e);
                    error_json("internal")
                }
            }
        }
        Command::GetClipboardText => {
            let cb = state.clipboard.clone();
            let result = tokio::task::spawn_blocking(move || clipboard::read_text(&cb)).await;
            match result {
                Ok(Ok(Some(t))) => clipboard_text_json(t),
                Ok(Ok(None)) => empty_json(),
                Ok(Err(e)) => error_json(clipboard::error_code(&e)),
                Err(_) => error_json("internal"),
            }
        }
        Command::GetClipboardImage => {
            let cb = state.clipboard.clone();
            let result =
                tokio::task::spawn_blocking(move || clipboard::read_image_png_base64(&cb)).await;
            match result {
                Ok(Ok(Some((mime, b64)))) => clipboard_image_json(mime, b64),
                Ok(Ok(None)) => empty_json(),
                Ok(Err(e)) => error_json(clipboard::error_code(&e)),
                Err(_) => error_json("internal"),
            }
        }
        Command::SetClipboardImage { data } => {
            if data.len() > clipboard::MAX_IMG_B64 {
                return error_json("too_large");
            }
            let cb = state.clipboard.clone();
            let result = tokio::task::spawn_blocking(move || {
                let bytes = clipboard::decode_base64(&data)?;
                clipboard::write_image_from_bytes(&cb, &bytes)
            })
            .await;
            match result {
                Ok(Ok(())) => ok_json(),
                Ok(Err(e)) => error_json(clipboard::error_code(&e)),
                Err(_) => error_json("internal"),
            }
        }
        Command::UploadStart { name, size, mime } => {
            if size > state.max_size {
                return error_json("too_large");
            }
            let _ = mime; // 协议保留字段，服务端写盘时不需要
            let clean = match file_transfer::sanitize_filename(&name) {
                Some(n) => n,
                None => return error_json("forbidden_name"),
            };
            let meta = UploadMeta {
                name: clean,
                size,
                created_at: std::time::Instant::now(),
            };
            let id = state.registry.register_upload(meta);
            let url = format!("/upload/{}?t={}", id, state.token);
            upload_ready_json(&id, &url)
        }
        Command::GetFile => {
            let cb = state.clipboard.clone();
            let result = tokio::task::spawn_blocking(move || clipboard::read_file_list(&cb)).await;
            match result {
                Ok(Ok(files)) if !files.is_empty() => {
                    let files_json: Vec<serde_json::Value> = files
                        .into_iter()
                        .map(|fm| {
                            let name = fm.name.clone();
                            let size = fm.size;
                            let mime = fm.mime.clone();
                            let meta = file_transfer::DownloadMeta {
                                path: fm.path,
                                name: name.clone(),
                                size,
                                mime: mime.clone(),
                                created_at: std::time::Instant::now(),
                            };
                            let id = state.registry.register_download(meta);
                            let url = format!("/download/{}?t={}", id, state.token);
                            serde_json::json!({
                                "name": name,
                                "size": size,
                                "mime": mime,
                                "url": url,
                            })
                        })
                        .collect();
                    file_list_json(files_json)
                }
                Ok(Ok(_)) => empty_json(),
                Ok(Err(e)) => error_json(clipboard::error_code(&e)),
                Err(_) => error_json("internal"),
            }
        }
        Command::Enter => inject_key_cmd(&state.enigo, enigo::Key::Return).await,
        Command::Tab => inject_key_cmd(&state.enigo, enigo::Key::Tab).await,
        Command::Backspace => inject_key_cmd(&state.enigo, enigo::Key::Backspace).await,
        Command::Copy => inject_copy_cmd(&state.enigo).await,
        Command::Paste => inject_paste_cmd(&state.enigo).await,
        Command::MouseMove { dx, dy } => inject_mouse_move_cmd(&state.enigo, dx, dy).await,
        Command::MouseClick { button } => {
            let btn = button.into();
            inject_mouse_button_cmd(&state.enigo, btn).await
        }
        Command::MousePress { button } => {
            let btn = button.into();
            inject_mouse_button_press_cmd(&state.enigo, btn).await
        }
        Command::MouseRelease { button } => {
            let btn = button.into();
            inject_mouse_button_release_cmd(&state.enigo, btn).await
        }
        Command::MouseScroll { dy } => inject_mouse_scroll_cmd(&state.enigo, dy).await,
    }
}

async fn inject_key_cmd(
    enigo: &Arc<Mutex<Enigo>>,
    key: enigo::Key,
) -> String {
    let enigo = enigo.clone();
    let result = tokio::task::spawn_blocking(move || inject::inject_key(&enigo, key)).await;
    match result {
        Ok(Ok(())) => ok_json(),
        Ok(Err(e)) => {
            eprintln!("[ws] 按键注入失败: {}", e);
            error_json("inject_failed")
        }
        Err(e) => {
            eprintln!("[ws] spawn_blocking join 失败: {}", e);
            error_json("internal")
        }
    }
}

async fn inject_mouse_move_cmd(enigo: &Arc<Mutex<Enigo>>, dx: i32, dy: i32) -> String {
    let enigo = enigo.clone();
    let result =
        tokio::task::spawn_blocking(move || inject::inject_mouse_move(&enigo, dx, dy)).await;
    match result {
        Ok(Ok(())) => ok_json(),
        Ok(Err(e)) => {
            eprintln!("[ws] 鼠标移动失败: {}", e);
            error_json("inject_failed")
        }
        Err(e) => {
            eprintln!("[ws] spawn_blocking join 失败: {}", e);
            error_json("internal")
        }
    }
}

async fn inject_mouse_button_cmd(enigo: &Arc<Mutex<Enigo>>, button: enigo::Button) -> String {
    let enigo = enigo.clone();
    let result =
        tokio::task::spawn_blocking(move || inject::inject_mouse_button(&enigo, button)).await;
    match result {
        Ok(Ok(())) => ok_json(),
        Ok(Err(e)) => {
            eprintln!("[ws] 鼠标点击失败: {}", e);
            error_json("inject_failed")
        }
        Err(e) => {
            eprintln!("[ws] spawn_blocking join 失败: {}", e);
            error_json("internal")
        }
    }
}

async fn inject_mouse_button_press_cmd(
    enigo: &Arc<Mutex<Enigo>>,
    button: enigo::Button,
) -> String {
    let enigo = enigo.clone();
    let result =
        tokio::task::spawn_blocking(move || inject::inject_mouse_button_press(&enigo, button))
            .await;
    match result {
        Ok(Ok(())) => ok_json(),
        Ok(Err(e)) => {
            eprintln!("[ws] 鼠标按下失败: {}", e);
            error_json("inject_failed")
        }
        Err(e) => {
            eprintln!("[ws] spawn_blocking join 失败: {}", e);
            error_json("internal")
        }
    }
}

async fn inject_mouse_button_release_cmd(
    enigo: &Arc<Mutex<Enigo>>,
    button: enigo::Button,
) -> String {
    let enigo = enigo.clone();
    let result =
        tokio::task::spawn_blocking(move || inject::inject_mouse_button_release(&enigo, button))
            .await;
    match result {
        Ok(Ok(())) => ok_json(),
        Ok(Err(e)) => {
            eprintln!("[ws] 鼠标抬起失败: {}", e);
            error_json("inject_failed")
        }
        Err(e) => {
            eprintln!("[ws] spawn_blocking join 失败: {}", e);
            error_json("internal")
        }
    }
}

async fn inject_mouse_scroll_cmd(enigo: &Arc<Mutex<Enigo>>, dy: i32) -> String {
    let enigo = enigo.clone();
    let result = tokio::task::spawn_blocking(move || {
        inject::inject_mouse_scroll(&enigo, dy, enigo::Axis::Vertical)
    })
    .await;
    match result {
        Ok(Ok(())) => ok_json(),
        Ok(Err(e)) => {
            eprintln!("[ws] 滚轮失败: {}", e);
            error_json("inject_failed")
        }
        Err(e) => {
            eprintln!("[ws] spawn_blocking join 失败: {}", e);
            error_json("internal")
        }
    }
}

async fn inject_copy_cmd(enigo: &Arc<Mutex<Enigo>>) -> String {
    let enigo = enigo.clone();
    let result = tokio::task::spawn_blocking(move || inject::inject_copy(&enigo)).await;
    match result {
        Ok(Ok(())) => ok_json(),
        Ok(Err(e)) => {
            eprintln!("[ws] 复制快捷键失败: {}", e);
            error_json("inject_failed")
        }
        Err(e) => {
            eprintln!("[ws] spawn_blocking join 失败: {}", e);
            error_json("internal")
        }
    }
}

async fn inject_paste_cmd(enigo: &Arc<Mutex<Enigo>>) -> String {
    let enigo = enigo.clone();
    let result = tokio::task::spawn_blocking(move || inject::inject_paste(&enigo)).await;
    match result {
        Ok(Ok(())) => ok_json(),
        Ok(Err(e)) => {
            eprintln!("[ws] 粘贴快捷键失败: {}", e);
            error_json("inject_failed")
        }
        Err(e) => {
            eprintln!("[ws] spawn_blocking join 失败: {}", e);
            error_json("internal")
        }
    }
}

fn truncate_in_place(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    s.truncate(boundary);
}

fn ok_json() -> String {
    r#"{"type":"ok"}"#.to_string()
}

fn empty_json() -> String {
    r#"{"type":"empty"}"#.to_string()
}

fn error_json(code: &str) -> String {
    format!(r#"{{"type":"error","code":"{}"}}"#, code)
}

fn clipboard_text_json(content: String) -> String {
    serde_json::json!({"type": "clipboard_text", "content": content}).to_string()
}

fn clipboard_image_json(mime: String, data: String) -> String {
    serde_json::json!({"type": "clipboard_image", "mime": mime, "data": data}).to_string()
}

fn upload_ready_json(id: &str, url: &str) -> String {
    serde_json::json!({"type": "upload_ready", "id": id, "url": url}).to_string()
}

fn file_list_json(files: Vec<serde_json::Value>) -> String {
    serde_json::json!({"type": "file_list", "files": files}).to_string()
}

fn server_info_json(name: &str) -> String {
    serde_json::json!({"type": "server_info", "name": name}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_command() {
        let cmd: Command = serde_json::from_str(r#"{"type":"text","value":"你好"}"#).unwrap();
        match cmd {
            Command::Text { value } => assert_eq!(value, "你好"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn parse_get_clipboard_text() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"get_clipboard_text"}"#).unwrap();
        assert!(matches!(cmd, Command::GetClipboardText));
    }

    #[test]
    fn parse_get_clipboard_image() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"get_clipboard_image"}"#).unwrap();
        assert!(matches!(cmd, Command::GetClipboardImage));
    }

    #[test]
    fn parse_set_clipboard_image() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"set_clipboard_image","data":"abcd"}"#).unwrap();
        match cmd {
            Command::SetClipboardImage { data } => assert_eq!(data, "abcd"),
            _ => panic!("expected SetClipboardImage"),
        }
    }

    #[test]
    fn parse_text_missing_value_fails() {
        let r: Result<Command, _> = serde_json::from_str(r#"{"type":"text"}"#);
        assert!(r.is_err());
    }

    #[test]
    fn parse_unknown_type_fails() {
        let r: Result<Command, _> = serde_json::from_str(r#"{"type":"unknown"}"#);
        assert!(r.is_err());
    }

    #[test]
    fn parse_set_clipboard_image_missing_data_fails() {
        let r: Result<Command, _> = serde_json::from_str(r#"{"type":"set_clipboard_image"}"#);
        assert!(r.is_err());
    }

    #[test]
    fn json_helpers_format_correctly() {
        assert_eq!(ok_json(), r#"{"type":"ok"}"#);
        assert_eq!(empty_json(), r#"{"type":"empty"}"#);
        assert_eq!(error_json("oops"), r#"{"type":"error","code":"oops"}"#);
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let mut s = "你好世界".to_string(); // 4 chars, 12 bytes (UTF-8)
        truncate_in_place(&mut s, 5);
        // 5 字节边界不在 char boundary，应该回退到 3（"你" 占 3 字节）
        assert_eq!(s, "你");
    }

    #[test]
    fn truncate_keeps_short_strings_intact() {
        let mut s = "abc".to_string();
        truncate_in_place(&mut s, 100);
        assert_eq!(s, "abc");
    }

    #[test]
    fn parse_mouse_move() {
        let cmd: Command = serde_json::from_str(r#"{"type":"mouse_move","dx":10,"dy":-5}"#).unwrap();
        match cmd {
            Command::MouseMove { dx, dy } => {
                assert_eq!(dx, 10);
                assert_eq!(dy, -5);
            }
            _ => panic!("expected MouseMove"),
        }
    }

    #[test]
    fn parse_mouse_click_left() {
        let cmd: Command = serde_json::from_str(r#"{"type":"mouse_click","button":"left"}"#).unwrap();
        assert!(matches!(cmd, Command::MouseClick { button: MouseButton::Left }));
    }

    #[test]
    fn parse_mouse_click_right() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"mouse_click","button":"right"}"#).unwrap();
        assert!(matches!(cmd, Command::MouseClick { button: MouseButton::Right }));
    }

    #[test]
    fn parse_mouse_click_middle() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"mouse_click","button":"middle"}"#).unwrap();
        assert!(matches!(cmd, Command::MouseClick { button: MouseButton::Middle }));
    }

    #[test]
    fn parse_mouse_click_unknown_button_fails() {
        let r: Result<Command, _> =
            serde_json::from_str(r#"{"type":"mouse_click","button":"foo"}"#);
        assert!(r.is_err());
    }

    #[test]
    fn parse_mouse_scroll() {
        let cmd: Command = serde_json::from_str(r#"{"type":"mouse_scroll","dy":3}"#).unwrap();
        match cmd {
            Command::MouseScroll { dy } => assert_eq!(dy, 3),
            _ => panic!("expected MouseScroll"),
        }
    }

    #[test]
    fn parse_mouse_press_left() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"mouse_press","button":"left"}"#).unwrap();
        assert!(matches!(cmd, Command::MousePress { button: MouseButton::Left }));
    }

    #[test]
    fn parse_mouse_release_right() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"mouse_release","button":"right"}"#).unwrap();
        assert!(matches!(cmd, Command::MouseRelease { button: MouseButton::Right }));
    }

    #[test]
    fn parse_mouse_press_middle() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"mouse_press","button":"middle"}"#).unwrap();
        assert!(matches!(cmd, Command::MousePress { button: MouseButton::Middle }));
    }

    #[test]
    fn mouse_button_to_enigo_mapping() {
        assert!(matches!(enigo::Button::from(MouseButton::Left), enigo::Button::Left));
        assert!(matches!(enigo::Button::from(MouseButton::Right), enigo::Button::Right));
        assert!(matches!(enigo::Button::from(MouseButton::Middle), enigo::Button::Middle));
    }

    #[test]
    fn parse_copy() {
        let cmd: Command = serde_json::from_str(r#"{"type":"copy"}"#).unwrap();
        assert!(matches!(cmd, Command::Copy));
    }

    #[test]
    fn parse_paste() {
        let cmd: Command = serde_json::from_str(r#"{"type":"paste"}"#).unwrap();
        assert!(matches!(cmd, Command::Paste));
    }
}
