use crate::cli::Args;
use crate::middleware::RateLimiter;
use std::sync::{Arc, RwLock};

pub struct AppState {
    pub args: Args,
    pub script_content: Arc<RwLock<Option<String>>>,
    pub rate_limiter: Option<RateLimiter>,
}
