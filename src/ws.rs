use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;

use crate::backend::{BackendError, DynBackend, InputBackend};
use crate::clipboard;
use crate::file_transfer::{self, UploadMeta};
use crate::state::AppState;

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
    /// 文件上传批结束后，前端把收集到的实际落盘文件名发回来，server 在 save_dir 下解析
    /// 路径后用 set().file_list() 一次推到剪贴板。不存在的 name 静默跳过。
    SetClipboardFiles { names: Vec<String> },
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
    _: crate::state::Authed,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    Ok(ws.on_upgrade(move |socket| handle_socket(socket, state)))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    tracing::info!("ws 客户端已连接");
    // 升级后立刻推送设备名 + 主题偏好，前端用于状态栏显示和主题应用
    let theme = state.theme.lock().clone();
    let info = server_info_json(&state.name, &theme);
    if socket.send(Message::Text(info.into())).await.is_err() {
        tracing::warn!("ws 发送 server_info 失败，断开");
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
    tracing::info!("ws 客户端断开");
}

async fn dispatch(state: &AppState, raw: &str) -> String {
    let cmd: Command = match serde_json::from_str(raw) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("ws 指令解析失败: {}", e);
            return error_json("decode_failed");
        }
    };
    let backend = state.backend.clone();
    match cmd {
        Command::Text { mut value } => {
            truncate_in_place(&mut value, MAX_TEXT_BYTES);
            spawn_inject(backend.clone(), move |b| b.inject_text(&value)).await
        }
        Command::GetClipboardText => {
            spawn_block(backend.clone(), |b| b.read_clipboard_text(), |res| match res {
                Some(t) => clipboard_text_json(t),
                None => empty_json(),
            })
            .await
        }
        Command::GetClipboardImage => {
            spawn_block(
                backend.clone(),
                |b| b.read_clipboard_image(),
                |res| match res {
                    Some((mime, b64)) => clipboard_image_json(mime, b64),
                    None => empty_json(),
                },
            )
            .await
        }
        Command::SetClipboardImage { data } => {
            if data.len() > clipboard::MAX_IMG_B64 {
                return error_json("too_large");
            }
            spawn_block(
                backend.clone(),
                move |b| {
                    let bytes = clipboard::decode_base64(&data)?;
                    b.write_clipboard_image(&bytes)
                },
                |_| ok_json(),
            )
            .await
        }
        Command::SetClipboardFiles { names } => {
            let save_dir = state.save_dir.clone();
            spawn_block(
                backend.clone(),
                move |b| {
                    // 前端只传文件名（绝对路径不暴露给浏览器），server 在 save_dir 下解析。
                    // 过滤掉不存在的（前端传错名 / 文件被外部删 / 旧批残留），剩下的推剪贴板。
                    let paths: Vec<std::path::PathBuf> = names
                        .iter()
                        .map(|n| save_dir.join(n))
                        .filter(|p| p.is_file())
                        .collect();
                    if paths.is_empty() {
                        return Ok(());
                    }
                    b.push_files_to_clipboard(&paths)
                },
                |_| ok_json(),
            )
            .await
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
            spawn_block(
                backend.clone(),
                |b| b.read_clipboard_files(),
                |files| {
                    if files.is_empty() {
                        empty_json()
                    } else {
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
                },
            )
            .await
        }
        Command::Enter => inject_key_cmd(&backend, enigo::Key::Return).await,
        Command::Tab => inject_key_cmd(&backend, enigo::Key::Tab).await,
        Command::Backspace => inject_key_cmd(&backend, enigo::Key::Backspace).await,
        Command::Copy => inject_copy_cmd(&backend).await,
        Command::Paste => inject_paste_cmd(&backend).await,
        Command::MouseMove { dx, dy } => {
            // 后端乘灵敏度系数：前端完全不感知，state.mouse_sensitivity 改了立即生效。
            // 不 clamp：dx/dy 是手机端相对位移（几像素到几十像素），即使 ×5.0 也远不到 i32 边界。
            let s = *state.mouse_sensitivity.lock();
            inject_mouse_move_cmd(&backend, (dx as f32 * s) as i32, (dy as f32 * s) as i32).await
        }
        Command::MouseClick { button } => {
            let btn = button.into();
            inject_mouse_button_cmd(&backend, btn).await
        }
        Command::MousePress { button } => {
            let btn = button.into();
            inject_mouse_button_press_cmd(&backend, btn).await
        }
        Command::MouseRelease { button } => {
            let btn = button.into();
            inject_mouse_button_release_cmd(&backend, btn).await
        }
        Command::MouseScroll { dy } => inject_mouse_scroll_cmd(&backend, dy).await,
    }
}

// ============================================================================
// spawn_blocking 调度 helper：消除「克隆 backend → spawn_blocking → 三段 match」样板
// ============================================================================

/// 把一个返回 `Result<(), BackendError>` 的阻塞注入操作调度到 blocking 线程池，收敛成协议 JSON。
///
/// 统一处理三种结果：
/// - `Ok(Ok(()))` → `ok_json()`
/// - `Ok(Err(e))` → 业务失败，记 warn 日志 + `error_json(e.error_code())`
/// - `Err(join_err)` → 线程池 panic，记 error 日志 + `error_json("internal")`
///
/// 所有键鼠/复制粘贴注入都走这条路径。`op` 闭包接收 `&dyn InputBackend`，
/// 这样调用方只需写 `|b| b.inject_xxx(...)`，backend 句柄由本函数注入。
async fn spawn_inject<F>(backend: DynBackend, op: F) -> String
where
    F: FnOnce(&dyn InputBackend) -> Result<(), BackendError> + Send + 'static,
{    match tokio::task::spawn_blocking(move || op(backend.as_ref())).await {
        Ok(Ok(())) => ok_json(),
        Ok(Err(e)) => {
            let code = e.error_code();
            tracing::warn!(error = ?e, code, "注入失败");
            error_json(code)
        }
        Err(e) => {
            tracing::error!(error = %e, "spawn_blocking join 失败");
            error_json("internal")
        }
    }
}

/// 把一个返回 `Result<T, BackendError>` 的阻塞剪贴板操作调度到 blocking 线程池，
/// 成功时用 `map` 把 `T` 转成协议 JSON，失败时映射成 `error_code`。
///
/// 与 `spawn_inject` 的区别：剪贴板操作有返回值（读到的文本/图片/文件列表），
/// 用 `map` 闭包把成功值转成协议 JSON。
async fn spawn_block<T, F, M>(
    backend: DynBackend,
    op: F,
    map: M,
) -> String
where
    F: FnOnce(&dyn InputBackend) -> Result<T, BackendError> + Send + 'static,
    T: Send + 'static,
    M: FnOnce(T) -> String,
{
    let result = tokio::task::spawn_blocking(move || op(backend.as_ref())).await;
    match result {
        Ok(Ok(t)) => map(t),
        Ok(Err(e)) => {
            let code = e.error_code();
            tracing::warn!(error = ?e, code, "剪贴板操作失败");
            error_json(code)
        }
        Err(e) => {
            tracing::error!(error = %e, "spawn_blocking join 失败");
            error_json("internal")
        }
    }
}

// ============================================================================
// 注入指令的薄封装：每个调用点只负责构造闭包，调度 + 错误收敛全由 spawn_inject 承担
// ============================================================================

async fn inject_key_cmd(backend: &DynBackend, key: enigo::Key) -> String {
    let backend = backend.clone();
    spawn_inject(backend, move |b| b.inject_key(key)).await
}

async fn inject_mouse_move_cmd(
    backend: &DynBackend,
    dx: i32,
    dy: i32,
) -> String {
    let backend = backend.clone();
    spawn_inject(backend, move |b| b.inject_mouse_move(dx, dy)).await
}

async fn inject_mouse_button_cmd(
    backend: &DynBackend,
    button: enigo::Button,
) -> String {
    let backend = backend.clone();
    spawn_inject(backend, move |b| b.inject_mouse_button(button)).await
}

async fn inject_mouse_button_press_cmd(
    backend: &DynBackend,
    button: enigo::Button,
) -> String {
    let backend = backend.clone();
    spawn_inject(backend, move |b| b.inject_mouse_button_press(button)).await
}

async fn inject_mouse_button_release_cmd(
    backend: &DynBackend,
    button: enigo::Button,
) -> String {
    let backend = backend.clone();
    spawn_inject(backend, move |b| b.inject_mouse_button_release(button)).await
}

async fn inject_mouse_scroll_cmd(backend: &DynBackend, dy: i32) -> String {
    let backend = backend.clone();
    spawn_inject(backend, move |b| b.inject_mouse_scroll(dy, enigo::Axis::Vertical)).await
}

async fn inject_copy_cmd(backend: &DynBackend) -> String {
    let backend = backend.clone();
    spawn_inject(backend, |b| b.inject_copy()).await
}

async fn inject_paste_cmd(backend: &DynBackend) -> String {
    let backend = backend.clone();
    spawn_inject(backend, |b| b.inject_paste()).await
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

fn server_info_json(name: &str, theme: &str) -> String {
    // version 用 env! 编译期内联 Cargo.toml 的 package.version，前端拿来做「当前版本」展示。
    // theme 是当前生效的主题偏好（"dark"/"light"/"system"），前端据此应用 [data-theme]。
    // 即便前端在 ws 连接前已经根据 prefers-color-scheme 渲染过一次，server_info 推过来后
    // 还会覆盖一次——保证用户在 qrctrl 内的显式选择生效。
    serde_json::json!({
        "type": "server_info",
        "name": name,
        "version": env!("CARGO_PKG_VERSION"),
        "theme": theme,
    })
    .to_string()
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

    // ========================================================================
    // MockBackend：记录所有调用，验证 dispatch 调度机制（spawn_inject/spawn_block）
    // 把 backend 方法调用 + 参数正确传递 + 错误收敛这三件事从「无法 mock 的真实 OS」
    // 里解耦出来。这是 dispatch 最容易出 bug 的部分（参数顺序、灵敏度乘法、错误码映射）。
    // ========================================================================

    use std::sync::Mutex;

    use crate::backend::{BackendError, FileMeta};
    use crate::clipboard::CbError;

    /// MockBackend 记录的方法调用，用于断言「dispatch 收到 X 指令后调用了 backend 的 Y 方法」。
    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        InjectText(String),
        InjectKey(String),
        InjectMouseMove(i32, i32),
        InjectMouseButton(String),
        InjectMouseButtonPress(String),
        InjectMouseButtonRelease(String),
        InjectMouseScroll(i32),
        InjectCopy,
        InjectPaste,
        ReadClipboardText,
        ReadClipboardImage,
        WriteClipboardImage,
        ReadClipboardFiles,
    }

    /// 测试用 InputBackend：记录所有调用到 `calls`，方法返回 `ret` 预设的结果。
    struct MockBackend {
        calls: Mutex<Vec<Call>>,
        /// 控制下一次调用的返回值：Ok(()) 成功 / Err(BackendError) 失败。
        /// 简化：所有方法返回同一个预设结果（足够测错误收敛路径）。
        fail: bool,
        /// read_clipboard_text 返回的预设文本。
        text: Option<String>,
    }

    impl MockBackend {
        fn record(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl InputBackend for MockBackend {
        fn inject_text(&self, text: &str) -> Result<(), BackendError> {
            self.record(Call::InjectText(text.to_string()));
            if self.fail {
                Err(BackendError::Inject("mock fail".into()))
            } else {
                Ok(())
            }
        }
        fn inject_key(&self, key: enigo::Key) -> Result<(), BackendError> {
            let label = format!("{:?}", key);
            self.record(Call::InjectKey(label));
            if self.fail {
                Err(BackendError::Inject("mock fail".into()))
            } else {
                Ok(())
            }
        }
        fn inject_mouse_move(&self, dx: i32, dy: i32) -> Result<(), BackendError> {
            self.record(Call::InjectMouseMove(dx, dy));
            if self.fail {
                Err(BackendError::Inject("mock fail".into()))
            } else {
                Ok(())
            }
        }
        fn inject_mouse_button(&self, button: enigo::Button) -> Result<(), BackendError> {
            self.record(Call::InjectMouseButton(format!("{:?}", button)));
            if self.fail {
                Err(BackendError::Inject("mock fail".into()))
            } else {
                Ok(())
            }
        }
        fn inject_mouse_button_press(&self, button: enigo::Button) -> Result<(), BackendError> {
            self.record(Call::InjectMouseButtonPress(format!("{:?}", button)));
            if self.fail {
                Err(BackendError::Inject("mock fail".into()))
            } else {
                Ok(())
            }
        }
        fn inject_mouse_button_release(&self, button: enigo::Button) -> Result<(), BackendError> {
            self.record(Call::InjectMouseButtonRelease(format!("{:?}", button)));
            if self.fail {
                Err(BackendError::Inject("mock fail".into()))
            } else {
                Ok(())
            }
        }
        fn inject_mouse_scroll(&self, amount: i32, _axis: enigo::Axis) -> Result<(), BackendError> {
            self.record(Call::InjectMouseScroll(amount));
            if self.fail {
                Err(BackendError::Inject("mock fail".into()))
            } else {
                Ok(())
            }
        }
        fn inject_copy(&self) -> Result<(), BackendError> {
            self.record(Call::InjectCopy);
            if self.fail {
                Err(BackendError::Inject("mock fail".into()))
            } else {
                Ok(())
            }
        }
        fn inject_paste(&self) -> Result<(), BackendError> {
            self.record(Call::InjectPaste);
            if self.fail {
                Err(BackendError::Inject("mock fail".into()))
            } else {
                Ok(())
            }
        }
        fn read_clipboard_text(&self) -> Result<Option<String>, BackendError> {
            self.record(Call::ReadClipboardText);
            if self.fail {
                Err(BackendError::Clipboard(CbError::ClipboardOccupied))
            } else {
                Ok(self.text.clone())
            }
        }
        fn read_clipboard_image(&self) -> Result<Option<(String, String)>, BackendError> {
            self.record(Call::ReadClipboardImage);
            if self.fail {
                Err(BackendError::Clipboard(CbError::ClipboardOccupied))
            } else {
                Ok(None)
            }
        }
        fn write_clipboard_image(&self, _bytes: &[u8]) -> Result<(), BackendError> {
            self.record(Call::WriteClipboardImage);
            if self.fail {
                Err(BackendError::Clipboard(CbError::ConversionFailure))
            } else {
                Ok(())
            }
        }
        fn read_clipboard_files(&self) -> Result<Vec<FileMeta>, BackendError> {
            self.record(Call::ReadClipboardFiles);
            if self.fail {
                Err(BackendError::Clipboard(CbError::ContentNotAvailable))
            } else {
                Ok(Vec::new())
            }
        }
        fn push_files_to_clipboard(
            &self,
            _paths: &[std::path::PathBuf],
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn mock_dyn(fail: bool) -> crate::backend::DynBackend {
        std::sync::Arc::new(MockBackend {
            calls: Mutex::new(Vec::new()),
            fail,
            text: None,
        })
    }

    /// spawn_inject 成功 → ok_json，且 backend 方法被调用。
    #[tokio::test]
    async fn spawn_inject_success_returns_ok() {
        let backend = mock_dyn(false);
        // 捕获 MockBackend 的 calls 引用：从 Arc 拿到 &MockBackend 需要先存一份弱引用方案太绕，
        // 改用：构造两个独立 backend——一个供 spawn_inject 消费，一个只用来读 calls。
        // 更简单的办法：spawn_inject 拿 Arc，我们 clone 一份 Arc 先存着。
        let probe = backend.clone();
        let json = spawn_inject(backend, |b| b.inject_text("hi")).await;
        assert_eq!(json, r#"{"type":"ok"}"#);
        // probe 和传入的 backend 是同一个 Arc，calls 共享。
        let mock = probe
            .as_ref()
            .as_any()
            .downcast_ref::<MockBackend>()
            .expect("downcast MockBackend");
        assert_eq!(mock.calls(), vec![Call::InjectText("hi".to_string())]);
    }

    /// spawn_inject 失败 → error_json(inject_failed)。
    #[tokio::test]
    async fn spawn_inject_failure_returns_inject_failed() {
        let backend = mock_dyn(true);
        let json = spawn_inject(backend, |b| b.inject_key(enigo::Key::Return)).await;
        assert_eq!(json, r#"{"type":"error","code":"inject_failed"}"#);
    }

    /// spawn_block 成功 → map 应用到返回值。
    #[tokio::test]
    async fn spawn_block_success_maps_result() {
        let backend: crate::backend::DynBackend = std::sync::Arc::new(MockBackend {
            calls: Mutex::new(Vec::new()),
            fail: false,
            text: Some("hello".to_string()),
        });
        let json = spawn_block(
            backend,
            |b| b.read_clipboard_text(),
            |res| match res {
                Some(t) => clipboard_text_json(t),
                None => empty_json(),
            },
        )
        .await;
        // serde_json::json! 用 BTreeMap 序列化，字段按字母序：content 在 type 前。
        assert_eq!(
            json,
            r#"{"content":"hello","type":"clipboard_text"}"#
        );
    }

    /// spawn_block 失败 → error_code 映射（ClipboardOccupied → clipboard_busy）。
    #[tokio::test]
    async fn spawn_block_failure_maps_error_code() {
        let backend = mock_dyn(true);
        let json = spawn_block(
            backend,
            |b| b.read_clipboard_text(),
            |res| match res {
                Some(t) => clipboard_text_json(t),
                None => empty_json(),
            },
        )
        .await;
        assert_eq!(
            json,
            r#"{"type":"error","code":"clipboard_busy"}"#
        );
    }

    /// spawn_block 返回空 → empty_json。
    #[tokio::test]
    async fn spawn_block_none_returns_empty() {
        let backend = mock_dyn(false);
        let json = spawn_block(
            backend,
            |b| b.read_clipboard_text(),
            |res| match res {
                Some(t) => clipboard_text_json(t),
                None => empty_json(),
            },
        )
        .await;
        assert_eq!(json, r#"{"type":"empty"}"#);
    }

    /// 鼠标移动 helper：参数透传（含灵敏度会在 dispatch 层乘，helper 本身原样传）。
    #[tokio::test]
    async fn inject_mouse_move_cmd_passes_params() {
        let backend = mock_dyn(false);
        let probe = backend.clone();
        let json = inject_mouse_move_cmd(&backend, 10, -5).await;
        assert_eq!(json, r#"{"type":"ok"}"#);
        let mock = probe.as_ref().as_any().downcast_ref::<MockBackend>().unwrap();
        assert_eq!(mock.calls(), vec![Call::InjectMouseMove(10, -5)]);
    }

    /// 滚轮 helper：参数 + 固定 Vertical 轴。
    #[tokio::test]
    async fn inject_mouse_scroll_cmd_passes_amount() {
        let backend = mock_dyn(false);
        let probe = backend.clone();
        let json = inject_mouse_scroll_cmd(&backend, 3).await;
        assert_eq!(json, r#"{"type":"ok"}"#);
        let mock = probe.as_ref().as_any().downcast_ref::<MockBackend>().unwrap();
        assert_eq!(mock.calls(), vec![Call::InjectMouseScroll(3)]);
    }

    /// Copy/Paste helper：无参数调用记录。
    #[tokio::test]
    async fn inject_copy_paste_cmd_records_calls() {
        let backend = mock_dyn(false);
        let probe = backend.clone();
        inject_copy_cmd(&backend).await;
        inject_paste_cmd(&backend).await;
        let mock = probe.as_ref().as_any().downcast_ref::<MockBackend>().unwrap();
        assert_eq!(mock.calls(), vec![Call::InjectCopy, Call::InjectPaste]);
    }
}
