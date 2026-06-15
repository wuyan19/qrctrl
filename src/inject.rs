use std::sync::{Arc, Mutex};

use enigo::{Enigo, Keyboard};

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
