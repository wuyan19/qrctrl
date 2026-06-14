use std::sync::{Arc, Mutex};

use enigo::Enigo;

#[derive(Clone)]
pub struct AppState {
    pub token: String,
    pub enigo: Arc<Mutex<Enigo>>,
}
