//! Anthropic messages API. Used with an API key (Anthropic, DeepSeek's
//! /anthropic endpoint, any compatible gateway) or with a Claude Pro/Max
//! subscription through Claude Code's OAuth login (kind = "claude").

use super::{Provider, Request, Response, api_key, check, http, openai::parse_args, sse, with_headers};
use crate::auth::Auth;
use crate::config::ProviderCfg;
use crate::types::{Event, Message, Sink, ToolCall};
use anyhow::{Result, bail};
use serde_json::{Value, json};

const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

pub struct Anthropic {
    cfg: ProviderCfg,
    base: String,
    key: Option<String>,
    oauth: Option<Auth>,
}

impl Anthropic {
    pub fn with_key(cfg: &ProviderCfg) -> Self {
        Anthropic {
            cfg: cfg.clone(),
            base: base(cfg),
            key: api_key(cfg).or_else(|| std::env::var("ANTHROPIC_API_KEY").ok()),
            oauth: None,
        }
    }

    pub fn with_oauth(cfg: &ProviderCfg, auth: Auth) -> Self {
        Anthropic { cfg: cfg.clone(), base: base(cfg), key: None, oauth: Some(auth) }
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        for attempt in 0..2 {
            let mut rb =
                http().post(format!("{}/v1/messages", self.base)).header("anthropic-version", "2023-06-01").json(body);
            if let Some(auth) = &self.oauth {
                let creds = if attempt == 0 { auth.token().await? } else { auth.refresh().await? };
                // Identity headers as Claude Code sends them (values from CLIProxyAPI).
                rb = rb
                    .bearer_auth(creds.access_token)
                    .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14")
                    .header("user-agent", "claude-cli/2.1.280 (external, cli)")
                    .header("x-app", "cli")
                    .header("x-stainless-lang", "js")
                    .header("x-stainless-runtime", "node")
                    .header("x-stainless-retry-count", "0")
                    .header("x-stainless-timeout", "600");
            } else if let Some(k) = &self.key {
                rb = rb.header("x-api-key", k);
            }
            let resp = with_headers(rb, &self.cfg).send().await?;
            if resp.status() == 401 && self.oauth.is_some() && attempt == 0 {
                continue;
            }
            return check(resp).await;
        }
        unreachable!()
    }

    fn system(&self, system: &str) -> Value {
        let mut blocks = Vec::new();
        if self.oauth.is_some() {
            // The subscription endpoint only serves requests that present themselves as Claude Code.
            blocks.push(json!({"type": "text", "text": CLAUDE_CODE_IDENTITY}));
        }
        if !system.is_empty() {
            blocks.push(json!({"type": "text", "text": system}));
        }
        json!(blocks)
    }
}

fn base(cfg: &ProviderCfg) -> String {
    cfg.base_url.clone().unwrap_or_else(|| "https://api.anthropic.com".into()).trim_end_matches('/').to_string()
}

pub fn to_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        let (role, block) = match m {
            Message::User { content } => ("user", vec![json!({"type": "text", "text": content})]),
            Message::Assistant { text, tool_calls, .. } => {
                let mut blocks = Vec::new();
                if let Some(t) = text.as_ref().filter(|t| !t.trim().is_empty()) {
                    blocks.push(json!({"type": "text", "text": t}));
                }
                for tc in tool_calls {
                    let input = if tc.arguments.is_object() { tc.arguments.clone() } else { json!({}) };
                    blocks.push(json!({"type": "tool_use", "id": tc.id, "name": tc.name, "input": input}));
                }
                ("assistant", blocks)
            }
            Message::Tool { call_id, content, .. } => {
                ("user", vec![json!({"type": "tool_result", "tool_use_id": call_id, "content": content})])
            }
        };
        if block.is_empty() {
            continue;
        }
        // Consecutive same-role turns are merged (tool results, retried prompts).
        match out.last_mut() {
            Some(last) if last["role"] == role => {
                last["content"].as_array_mut().unwrap().extend(block);
            }
            _ => out.push(json!({"role": role, "content": block})),
        }
    }
    out
}

#[async_trait::async_trait]
impl Provider for Anthropic {
    async fn complete(&self, req: Request<'_>, sink: &dyn Sink) -> Result<Response> {
        let mut body = json!({
            "model": self.cfg.model,
            "max_tokens": self.cfg.max_tokens.unwrap_or(16000),
            "system": self.system(req.system),
            "messages": to_messages(req.messages),
            "stream": true,
        });
        if !req.tools.is_empty() {
            body["tools"] = req
                .tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters}))
                .collect();
        }
        let resp = self.post(&body).await?;

        let mut text = String::new();
        // index -> (kind, id, name, json)
        let mut blocks: Vec<(String, String, String, String)> = Vec::new();
        sse(resp, |_, data| {
            let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(()) };
            match v["type"].as_str().unwrap_or("") {
                "content_block_start" => {
                    let idx = v["index"].as_u64().unwrap_or(blocks.len() as u64) as usize;
                    while blocks.len() <= idx {
                        blocks.push(Default::default());
                    }
                    let cb = &v["content_block"];
                    blocks[idx] = (
                        cb["type"].as_str().unwrap_or("").into(),
                        cb["id"].as_str().unwrap_or("").into(),
                        cb["name"].as_str().unwrap_or("").into(),
                        String::new(),
                    );
                }
                "content_block_delta" => {
                    let idx = v["index"].as_u64().unwrap_or(0) as usize;
                    let d = &v["delta"];
                    if let Some(t) = d["text"].as_str() {
                        sink.emit(Event::Text { content: t.to_string() });
                        text.push_str(t);
                    }
                    if let (Some(j), Some(b)) = (d["partial_json"].as_str(), blocks.get_mut(idx)) {
                        b.3.push_str(j);
                    }
                }
                "error" => bail!("{}", v["error"]["message"].as_str().unwrap_or(data)),
                _ => {}
            }
            Ok(())
        })
        .await?;

        let tool_calls = blocks
            .into_iter()
            .filter(|b| b.0 == "tool_use")
            .map(|(_, id, name, args)| ToolCall { id, name, arguments: parse_args(&args), meta: None })
            .collect();
        Ok(Response { text: Some(text).filter(|t| !t.is_empty()), tool_calls, reasoning: None })
    }

    fn has_web_search(&self) -> bool {
        self.oauth.is_some() || self.base.contains("api.anthropic.com")
    }

    async fn web_search(&self, query: &str) -> Result<String> {
        let body = json!({
            "model": self.cfg.model,
            "max_tokens": 4096,
            "system": self.system(""),
            "messages": [{"role": "user", "content": format!("{}{query}", super::SEARCH_PROMPT)}],
            "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 5}],
        });
        let v: Value = self.post(&body).await?.json().await?;
        let mut text = String::new();
        let mut sources = Vec::new();
        for b in v["content"].as_array().into_iter().flatten() {
            if b["type"] == "text" {
                text.push_str(b["text"].as_str().unwrap_or(""));
                for c in b["citations"].as_array().into_iter().flatten() {
                    sources.push((c["title"].as_str().unwrap_or("").into(), c["url"].as_str().unwrap_or("").into()));
                }
            }
        }
        Ok(super::with_sources(text, &sources))
    }
}
