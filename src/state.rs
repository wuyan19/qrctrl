use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use enigo::Enigo;
use serde::Deserialize;

use crate::clipboard::ClipboardHandle;
use crate::file_transfer::TransferRegistry;

/// HTTP / WebSocket 共用的 token 查询参数。
#[derive(Deserialize)]
pub struct TokenQuery {
    pub t: String,
}

#[derive(Clone)]
pub struct AppState {
    pub token: String,
    pub name: String,
    pub enigo: Arc<Mutex<Enigo>>,
    pub clipboard: ClipboardHandle,
    pub save_dir: PathBuf,
    pub max_size: u64,
    pub registry: TransferRegistry,
}
