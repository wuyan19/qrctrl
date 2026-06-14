use std::sync::{Arc, Mutex};

use enigo::{Enigo, Keyboard};

/// 把文本注入到当前焦点窗口。在 blocking 线程里调用。
pub fn inject_text(enigo: &Arc<Mutex<Enigo>>, text: &str) -> Result<(), String> {
    let mut e = enigo.lock().map_err(|e| format!("lock error: {}", e))?;
    e.text(text).map_err(|e| format!("inject error: {}", e))
}
