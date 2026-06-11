pub mod openai;

#[cfg(feature = "anthropic")]
pub mod anthropic;

use proto::{LlmBackend, LlmConfig};
use std::sync::Arc;

/// Create a built-in backend by provider name.
/// Returns None for unknown providers — caller falls back to openai-compatible.
pub fn create(cfg: &LlmConfig, api_key: Option<String>) -> Option<Arc<dyn LlmBackend>> {
    match cfg.provider.as_str() {
        #[cfg(feature = "anthropic")]
        "anthropic" => Some(Arc::new(anthropic::Anthropic::new(cfg, api_key))),
        "openai" => Some(Arc::new(openai::OpenAI::new(cfg, api_key))),
        _ => None,
    }
}
