use crate::cli::Args;
use std::sync::{Arc, RwLock};

pub struct AppState {
    pub args: Args,
    pub script_content: Arc<RwLock<Option<String>>>,
}
