use std::sync::{Arc, Mutex};

use enigo::{Axis, Button, Coordinate, Enigo, Keyboard, Mouse};

/// 把文本注入到当前焦点窗口。在 blocking 线程里调用。
pub fn inject_text(enigo: &Arc<Mutex<Enigo>>, text: &str) -> Result<(), String> {
    let mut e = enigo.lock().map_err(|e| format!("lock error: {}", e))?;
    e.text(text).map_err(|e| format!("inject error: {}", e))
}

/// 注入一次物理按键（按下 + 抬起）。在 blocking 线程里调用。
///
/// 用 enigo 的 `key()` 路径（基于虚拟键码 / keysym），与 `text()` 的 Unicode
/// 注入路径不同：本函数走系统键盘布局，专门用于功能键（Enter / Tab / Backspace 等），
/// 不用于打字。
pub fn inject_key(enigo: &Arc<Mutex<Enigo>>, key: enigo::Key) -> Result<(), String> {
    let mut e = enigo.lock().map_err(|e| format!("lock error: {}", e))?;
    e.key(key, enigo::Direction::Click)
        .map_err(|e| format!("key error: {}", e))
}

/// 相对移动鼠标光标。dx / dy 单位是像素，可为负数。
pub fn inject_mouse_move(enigo: &Arc<Mutex<Enigo>>, dx: i32, dy: i32) -> Result<(), String> {
    let mut e = enigo.lock().map_err(|e| format!("lock error: {}", e))?;
    e.move_mouse(dx, dy, Coordinate::Rel)
        .map_err(|e| format!("mouse move error: {}", e))
}

/// 点击鼠标按钮（按下 + 抬起）。
pub fn inject_mouse_button(enigo: &Arc<Mutex<Enigo>>, button: Button) -> Result<(), String> {
    let mut e = enigo.lock().map_err(|e| format!("lock error: {}", e))?;
    e.button(button, enigo::Direction::Click)
        .map_err(|e| format!("mouse button error: {}", e))
}

/// 滚动鼠标滚轮。amount 为正向下 / 向右，为负向上 / 向左。
pub fn inject_mouse_scroll(enigo: &Arc<Mutex<Enigo>>, amount: i32, axis: Axis) -> Result<(), String> {
    let mut e = enigo.lock().map_err(|e| format!("lock error: {}", e))?;
    e.scroll(amount, axis)
        .map_err(|e| format!("mouse scroll error: {}", e))
}
