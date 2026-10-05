use std::sync::Arc;

use enigo::{Axis, Button, Coordinate, Enigo, Keyboard, Mouse};

/// parking_lot::lock() 不返回 Result（不会 poison），所以不需要 .map_err。
/// guard 用完自动释放。
type LockedEnigo<'a> = parking_lot::MutexGuard<'a, Enigo>;

/// 把文本注入到当前焦点窗口。在 blocking 线程里调用。
pub fn inject_text(enigo: &Arc<parking_lot::Mutex<Enigo>>, text: &str) -> Result<(), String> {
    let mut e: LockedEnigo = enigo.lock();
    e.text(text).map_err(|e| format!("inject error: {}", e))
}

/// 注入一次物理按键（按下 + 抬起）。在 blocking 线程里调用。
///
/// 用 enigo 的 `key()` 路径（基于虚拟键码 / keysym），与 `text()` 的 Unicode
/// 注入路径不同：本函数走系统键盘布局，专门用于功能键（Enter / Tab / Backspace 等），
/// 不用于打字。
pub fn inject_key(enigo: &Arc<parking_lot::Mutex<Enigo>>, key: enigo::Key) -> Result<(), String> {
    let mut e = enigo.lock();
    e.key(key, enigo::Direction::Click)
        .map_err(|e| format!("key error: {}", e))
}

/// 相对移动鼠标光标。dx / dy 单位是像素，可为负数。
pub fn inject_mouse_move(
    enigo: &Arc<parking_lot::Mutex<Enigo>>,
    dx: i32,
    dy: i32,
) -> Result<(), String> {
    let mut e = enigo.lock();
    e.move_mouse(dx, dy, Coordinate::Rel)
        .map_err(|e| format!("mouse move error: {}", e))
}

/// 点击鼠标按钮（按下 + 抬起）。
pub fn inject_mouse_button(
    enigo: &Arc<parking_lot::Mutex<Enigo>>,
    button: Button,
) -> Result<(), String> {
    let mut e = enigo.lock();
    e.button(button, enigo::Direction::Click)
        .map_err(|e| format!("mouse button error: {}", e))
}

/// 按下鼠标按钮（不抬起）。用于拖拽手势的开始。
pub fn inject_mouse_button_press(
    enigo: &Arc<parking_lot::Mutex<Enigo>>,
    button: Button,
) -> Result<(), String> {
    let mut e = enigo.lock();
    e.button(button, enigo::Direction::Press)
        .map_err(|e| format!("mouse press error: {}", e))
}

/// 抬起鼠标按钮。用于拖拽手势的结束。
pub fn inject_mouse_button_release(
    enigo: &Arc<parking_lot::Mutex<Enigo>>,
    button: Button,
) -> Result<(), String> {
    let mut e = enigo.lock();
    e.button(button, enigo::Direction::Release)
        .map_err(|e| format!("mouse release error: {}", e))
}

/// 滚动鼠标滚轮。amount 为正向下 / 向右，为负向上 / 向左。
pub fn inject_mouse_scroll(
    enigo: &Arc<parking_lot::Mutex<Enigo>>,
    amount: i32,
    axis: Axis,
) -> Result<(), String> {
    let mut e = enigo.lock();
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
/// macOS 上不能用 `Key::Unicode`：enigo 把字符反查成键码
/// （`get_layoutdependent_keycode`）时要遍历 128 个键码逐个调 HIToolbox 的
/// TIS API，而 macOS 13.4+ 给 InputSourceKit 加了主线程 dispatch 断言，
/// 从 tokio blocking 线程调用会直接 SIGILL（崩溃栈：
/// `dispatch_assert_queue_fail → islGetInputSourceListWithAdditions`）。
/// 改用 `Key::Other` 传 ANSI 虚拟键码，enigo 原样透传、不查布局。
/// C=0x08 / V=0x09 是 QWERTY/AZERTY 系布局的通用位置；Dvorak 等重排布局上
/// 快捷键可能不生效（表现为粘不出，不会崩溃）。其他平台无此断言，保留
/// Unicode 路径（由 enigo 按各自布局映射）。
fn inject_shortcut(enigo: &Arc<parking_lot::Mutex<Enigo>>, ch: char) -> Result<(), String> {
    // 先解析键码再按压修饰键：未知字符在按压前就失败，不会卡住修饰键
    #[cfg(target_os = "macos")]
    let key = ansi_shortcut_keycode(ch)?;
    #[cfg(not(target_os = "macos"))]
    let key = enigo::Key::Unicode(ch);

    let mut e = enigo.lock();
    let modifier = platform_copy_paste_modifier();

    let press = e.key(modifier, enigo::Direction::Press);
    let click = e.key(key, enigo::Direction::Click);
    // 不论 press / click 是否成功，都尝试释放修饰键
    let _ = e.key(modifier, enigo::Direction::Release);

    press.map_err(|e| format!("modifier press error: {}", e))?;
    click.map_err(|e| format!("key click error: {}", e))?;
    Ok(())
}

/// macOS 下把快捷键字符映射成 ANSI 虚拟键码（kVK_ANSI_C / kVK_ANSI_V），
/// 绕开 enigo 的 TIS 布局反查——见 inject_shortcut 上的崩溃说明。
/// 只接受白名单字符：未知字符显式报错，**绝不回退 `Key::Unicode`**——
/// 那条路径会重新触发 TIS 主线程断言（SIGILL）。
#[cfg(target_os = "macos")]
fn ansi_shortcut_keycode(ch: char) -> Result<enigo::Key, String> {
    match ch {
        'c' => Ok(enigo::Key::Other(0x08)),
        'v' => Ok(enigo::Key::Other(0x09)),
        _ => Err(format!("unsupported shortcut char: {ch}")),
    }
}

pub fn inject_copy(enigo: &Arc<parking_lot::Mutex<Enigo>>) -> Result<(), String> {
    inject_shortcut(enigo, 'c')
}

pub fn inject_paste(enigo: &Arc<parking_lot::Mutex<Enigo>>) -> Result<(), String> {
    inject_shortcut(enigo, 'v')
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// 固化 C/V 的 ANSI 键码映射：`Key::Other` 由 enigo 原样透传成键码、
    /// 不经过 TIS 布局反查（这是绕开 macOS 13.4+ 主线程断言的关键，
    /// 见 inject_shortcut 注释）。常量写错快捷键就静默失效，用测试钉住。
    #[test]
    fn ansi_shortcut_keycodes_are_stable() {
        assert!(matches!(
            ansi_shortcut_keycode('c'),
            Ok(enigo::Key::Other(0x08))
        ));
        assert!(matches!(
            ansi_shortcut_keycode('v'),
            Ok(enigo::Key::Other(0x09))
        ));
        // 未知字符必须显式失败——回退 Key::Unicode 会重新触发 TIS 主线程断言
        assert!(ansi_shortcut_keycode('x').is_err());
    }
}
