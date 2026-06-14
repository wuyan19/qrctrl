//! enigo / arboard 跨版本升级的 Send/Sync 回归保险。
//! 如果未来 enigo 或 arboard 改了底层实现（例如新增 raw pointer 字段），
//! 这些断言会立即报错，让我们重新审视 axum State 用法。

use enigo::Enigo;
use std::sync::{Arc, Mutex};

fn _assert_send<T: Send>() {}
fn _assert_sync<T: Sync>() {}

#[test]
fn arc_mutex_enigo_is_send_sync() {
    // axum AppState 必须满足的边界
    _assert_send::<Arc<Mutex<Enigo>>>();
    _assert_sync::<Arc<Mutex<Enigo>>>();
    // AppState 本身也要 Clone + Send + Sync
    // (Arc<T> 总是 Clone，所以这等价于上面两个断言)
}

#[test]
fn enigo_is_send() {
    // 注意：macOS/Linux 上 Enigo 只有 Send 没有 Sync。
    // 这里只断言 Send，保持跨平台兼容。
    _assert_send::<Enigo>();
}

#[test]
fn arc_mutex_arboard_clipboard_is_send_sync() {
    // arboard::Clipboard 在 macOS 上同样仅 Send 不 Sync，
    // 与 enigo 一致，用 Arc<Mutex<..>> 包裹后 Send + Sync 都满足。
    _assert_send::<Arc<Mutex<arboard::Clipboard>>>();
    _assert_sync::<Arc<Mutex<arboard::Clipboard>>>();
}
