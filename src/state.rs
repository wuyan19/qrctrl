use std::sync::{Arc, Mutex};

use enigo::Enigo;

use crate::clipboard::ClipboardHandle;

#[derive(Clone)]
pub struct AppState {
    pub token: String,
    pub enigo: Arc<Mutex<Enigo>>,
    pub clipboard: ClipboardHandle,
}
