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

/// 按下鼠标按钮（不抬起）。用于拖拽手势的开始。
pub fn inject_mouse_button_press(enigo: &Arc<Mutex<Enigo>>, button: Button) -> Result<(), String> {
    let mut e = enigo.lock().map_err(|e| format!("lock error: {}", e))?;
    e.button(button, enigo::Direction::Press)
        .map_err(|e| format!("mouse press error: {}", e))
}

/// 抬起鼠标按钮。用于拖拽手势的结束。
pub fn inject_mouse_button_release(enigo: &Arc<Mutex<Enigo>>, button: Button) -> Result<(), String> {
    let mut e = enigo.lock().map_err(|e| format!("lock error: {}", e))?;
    e.button(button, enigo::Direction::Release)
        .map_err(|e| format!("mouse release error: {}", e))
}

/// 滚动鼠标滚轮。amount 为正向下 / 向右，为负向上 / 向左。
pub fn inject_mouse_scroll(enigo: &Arc<Mutex<Enigo>>, amount: i32, axis: Axis) -> Result<(), String> {
    let mut e = enigo.lock().map_err(|e| format!("lock error: {}", e))?;
    e.scroll(amount, axis)
        .map_err(|e| format!("mouse scroll error: {}", e))
}

/// 取当前平台的「主修饰键」：macOS 是 Cmd（Meta），其他平台是 Ctrl。
/// 用于 Copy / Paste 等剪贴板快捷键。
#[cfg(target_os = "macos")]
fn platform_copy_paste_modifier() -> enigo::Key {
    enigo::Key::Meta
}
#[cfg(not(target_os = "macos"))]
fn platform_copy_paste_modifier() -> enigo::Key {
    enigo::Key::Control
}

/// 注入「主修饰键 + 字符」组合（Press → Click → Release）。
/// 用于 Copy (Ctrl/Cmd + C) / Paste (Ctrl/Cmd + V)。
///
/// 即使中途失败，也保证释放修饰键，避免按键卡住（否则用户的真实键盘
/// 会一直「按住 Ctrl/Cmd」，后续所有按键都变成快捷键）。
fn inject_shortcut(enigo: &Arc<Mutex<Enigo>>, ch: char) -> Result<(), String> {
    let mut e = enigo.lock().map_err(|e| format!("lock error: {}", e))?;
    let modifier = platform_copy_paste_modifier();

    let press = e.key(modifier, enigo::Direction::Press);
    let click = e.key(enigo::Key::Unicode(ch), enigo::Direction::Click);
    // 不论 press / click 是否成功，都尝试释放修饰键
    let _ = e.key(modifier, enigo::Direction::Release);

    press.map_err(|e| format!("modifier press error: {}", e))?;
    click.map_err(|e| format!("key click error: {}", e))?;
    Ok(())
}

pub fn inject_copy(enigo: &Arc<Mutex<Enigo>>) -> Result<(), String> {
    inject_shortcut(enigo, 'c')
}

pub fn inject_paste(enigo: &Arc<Mutex<Enigo>>) -> Result<(), String> {
    inject_shortcut(enigo, 'v')
}
