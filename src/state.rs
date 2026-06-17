use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use enigo::Enigo;
use serde::Deserialize;
use tao::event_loop::EventLoopProxy;
use tokio::sync::Notify;

use crate::clipboard::ClipboardHandle;
use crate::file_transfer::TransferRegistry;
use crate::tray::UserEvent;

/// HTTP / WebSocket 共用的 token 查询参数。
#[derive(Deserialize)]
pub struct TokenQuery {
    pub t: String,
}

#[derive(Clone)]
pub struct AppState {
    pub token: String,
    pub name: String,
    pub addr: String,
    pub port: u16,
    pub prefer_ip: Option<String>,
    pub enigo: Arc<Mutex<Enigo>>,
    pub clipboard: ClipboardHandle,
    pub save_dir: PathBuf,
    pub max_size: u64,
    pub registry: TransferRegistry,
    /// 触发 server + tray 优雅 shutdown。restart 和 quit 都通过它发起。
    pub shutdown_notify: Arc<Notify>,
    /// 给 server 线程用来唤醒 tao event loop（restart 信号需要让 tray loop 退出）。
    pub tray_proxy: EventLoopProxy<UserEvent>,
}
