use anyhow::Result;
use proto::{LlmBackend, LlmConfig};
use std::sync::Arc;

pub mod builtin;
pub mod external;

/// Create an LLM backend from config.
/// External scripts take priority; unknown providers fall back to OpenAI-compatible.
pub fn create(cfg: &LlmConfig, api_key: Option<String>) -> Result<Arc<dyn LlmBackend>> {
    if cfg.provider == "external" {
        return Ok(Arc::new(external::External::new(cfg)?));
    }

    Ok(builtin::create(cfg, api_key.clone())
        .unwrap_or_else(|| Arc::new(builtin::openai::OpenAI::new(cfg, api_key))))
}
