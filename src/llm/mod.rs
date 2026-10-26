//! Model backends. Two generic API dialects (OpenAI chat completions, Anthropic
//! messages) plus unofficial adapters for subscription CLIs (Codex, Claude Code,
//! Gemini CLI, Antigravity).

use crate::auth::Auth;
use crate::config::{Config, ProviderCfg};
use crate::types::{Message, Sink, Tool, ToolCall};
use anyhow::{bail, Result};
use futures::StreamExt;
use std::sync::{Arc, OnceLock};

pub mod anthropic;
pub mod codex;
pub mod gemini;
pub mod openai;

pub struct Request<'a> {
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [Tool],
}

#[derive(Debug, Default)]
pub struct Response {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub reasoning: Option<String>,
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// One model turn. Text is streamed into `sink` as it arrives.
    async fn complete(&self, req: Request<'_>, sink: &dyn Sink) -> Result<Response>;

    /// The provider's own web search (only the native model can use it).
    async fn web_search(&self, _query: &str) -> Result<String> {
        bail!("this provider has no native web search")
    }

    fn has_web_search(&self) -> bool {
        false
    }
}

pub fn build(cfg: &Config, name: &str, model: Option<&str>) -> Result<Arc<dyn Provider>> {
    let mut p: ProviderCfg = cfg.provider(name)?.clone();
    if let Some(m) = model {
        p.model = m.to_string();
    }
    let auth = |kind: &str| Auth::new(cfg, name, kind, &p);
    Ok(match p.kind.as_str() {
        "openai" => Arc::new(openai::OpenAi::new(&p)),
        "anthropic" => Arc::new(anthropic::Anthropic::with_key(&p)),
        "claude" => Arc::new(anthropic::Anthropic::with_oauth(&p, auth("claude")?)),
        "codex" => Arc::new(codex::Codex::new(cfg, &p, auth("codex")?)),
        "gemini" | "antigravity" => Arc::new(gemini::Gemini::new(&p, auth(&p.kind)?)),
        other => bail!("provider '{name}': unknown kind '{other}'"),
    })
}

pub fn api_key(p: &ProviderCfg) -> Option<String> {
    p.api_key.clone().or_else(|| p.api_key_env.as_ref().and_then(|v| std::env::var(v).ok()))
}

pub fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(30)).build().expect("http client")
    })
}

pub fn with_headers(mut rb: reqwest::RequestBuilder, p: &ProviderCfg) -> reqwest::RequestBuilder {
    for (k, v) in &p.headers {
        rb = rb.header(k, v);
    }
    rb
}

/// Fail with the body text on a non-2xx status.
pub async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    bail!("HTTP {status}: {}", body.trim())
}

/// Read a server-sent-events body, calling `on(event, data)` per message.
pub async fn sse(resp: reqwest::Response, mut on: impl FnMut(&str, &str) -> Result<()>) -> Result<()> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut event = String::new();
    let mut data = String::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !data.is_empty() {
                    on(&event, &data)?;
                }
                event.clear();
                data.clear();
            } else if let Some(v) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(v.strip_prefix(' ').unwrap_or(v));
            } else if let Some(v) = line.strip_prefix("event:") {
                event = v.trim().to_string();
            }
        }
    }
    if !data.is_empty() {
        on(&event, &data)?;
    }
    Ok(())
}

/// Turn a list of (title, url) into a "Sources:" footer.
pub fn with_sources(text: String, sources: &[(String, String)]) -> String {
    let mut seen = std::collections::HashSet::new();
    let list: Vec<String> = sources
        .iter()
        .filter(|(_, url)| !url.is_empty() && seen.insert(url.clone()))
        .map(|(title, url)| if title.is_empty() { format!("- {url}") } else { format!("- {title}: {url}") })
        .collect();
    if list.is_empty() {
        text
    } else {
        format!("{}\n\nSources:\n{}", text.trim_end(), list.join("\n"))
    }
}

pub const SEARCH_PROMPT: &str = "Search the web and answer the query below. Be concise and factual, \
include concrete details (names, numbers, dates) and cite where they come from.\n\nQuery: ";
