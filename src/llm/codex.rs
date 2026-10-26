//! ChatGPT Plus/Pro subscription through the Codex backend (Responses API at
//! chatgpt.com/backend-api/codex). Unofficial: mimics the Codex CLI.

use super::{check, http, openai::parse_args, sse, with_headers, Provider, Request, Response};
use crate::auth::Auth;
use crate::config::{Config, ProviderCfg};
use crate::types::{new_id, Event, Message, Sink, ToolCall};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

const INSTRUCTIONS_URL: &str = "https://raw.githubusercontent.com/openai/codex/main/codex-rs/core/prompt.md";

pub struct Codex {
    cfg: ProviderCfg,
    auth: Auth,
    base: String,
    session: String,
    cache: PathBuf,
    /// Set once the backend rejected custom instructions; ours then go in a developer message.
    official_prompt: AtomicBool,
}

impl Codex {
    pub fn new(cfg: &Config, p: &ProviderCfg, auth: Auth) -> Self {
        Codex {
            cfg: p.clone(),
            auth,
            base: p
                .base_url
                .clone()
                .unwrap_or_else(|| "https://chatgpt.com/backend-api/codex".into())
                .trim_end_matches('/')
                .to_string(),
            session: new_id(),
            cache: cfg.path("cache").join("codex-instructions.md"),
            official_prompt: AtomicBool::new(false),
        }
    }

    async fn official_instructions(&self) -> Result<String> {
        if let Ok(s) = std::fs::read_to_string(&self.cache) {
            return Ok(s);
        }
        let text = check(http().get(INSTRUCTIONS_URL).send().await?).await?.text().await?;
        std::fs::create_dir_all(self.cache.parent().unwrap()).ok();
        std::fs::write(&self.cache, &text).ok();
        Ok(text)
    }

    /// POST /responses, handling token refresh and the instructions check.
    async fn post(&self, system: &str, input: Vec<Value>, tools: Vec<Value>) -> Result<reqwest::Response> {
        let mut refreshed = false;
        loop {
            let official = self.official_prompt.load(Ordering::Relaxed);
            let instructions = if official { self.official_instructions().await? } else { system.to_string() };
            let mut items = input.clone();
            if official && !system.is_empty() {
                items.insert(
                    0,
                    json!({"type": "message", "role": "developer",
                    "content": [{"type": "input_text", "text": system}]}),
                );
            }
            let mut body = json!({
                "model": self.cfg.model,
                "instructions": instructions,
                "input": items,
                "tools": tools,
                "tool_choice": "auto",
                "parallel_tool_calls": true,
                "store": false,
                "stream": true,
                "prompt_cache_key": self.session,
            });
            if let Some(e) = &self.cfg.reasoning_effort {
                body["reasoning"] = json!({"effort": e});
            }
            let creds = if refreshed { self.auth.refresh().await? } else { self.auth.token().await? };
            let mut rb = http()
                .post(format!("{}/responses", self.base))
                .bearer_auth(&creds.access_token)
                .header("OpenAI-Beta", "responses=experimental")
                .header("originator", "codex_cli_rs")
                .header("session_id", &self.session)
                .header("accept", "text/event-stream")
                .json(&body);
            if let Some(acc) = &creds.account_id {
                rb = rb.header("chatgpt-account-id", acc);
            }
            let resp = with_headers(rb, &self.cfg).send().await?;
            if resp.status() == 401 && !refreshed {
                refreshed = true;
                continue;
            }
            if resp.status() == 400 && !official {
                let text = resp.text().await.unwrap_or_default();
                if text.to_lowercase().contains("instructions") {
                    self.official_prompt.store(true, Ordering::Relaxed);
                    continue;
                }
                bail!("HTTP 400: {text}");
            }
            return check(resp).await;
        }
    }
}

fn to_input(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        match m {
            Message::User { content } => out.push(json!({
                "type": "message", "role": "user", "content": [{"type": "input_text", "text": content}]})),
            Message::Assistant { text, tool_calls, .. } => {
                if let Some(t) = text.as_ref().filter(|t| !t.is_empty()) {
                    out.push(json!({"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": t}]}));
                }
                for tc in tool_calls {
                    out.push(json!({"type": "function_call", "call_id": tc.id, "name": tc.name,
                        "arguments": tc.arguments.to_string()}));
                }
            }
            Message::Tool { call_id, content, .. } => {
                out.push(json!({"type": "function_call_output", "call_id": call_id, "output": content}))
            }
        }
    }
    out
}

fn failure(v: &Value) -> String {
    let e = if v["response"]["error"].is_object() { &v["response"]["error"] } else { &v["error"] };
    e["message"].as_str().map(String::from).unwrap_or_else(|| v.to_string())
}

#[async_trait::async_trait]
impl Provider for Codex {
    async fn complete(&self, req: Request<'_>, sink: &dyn Sink) -> Result<Response> {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "name": t.name, "description": t.description,
                "strict": false, "parameters": t.parameters})
            })
            .collect();
        let resp = self.post(req.system, to_input(req.messages), tools).await?;
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        sse(resp, |_, data| {
            let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(()) };
            match v["type"].as_str().unwrap_or("") {
                "response.output_text.delta" => {
                    let d = v["delta"].as_str().unwrap_or("");
                    sink.emit(Event::Text { content: d.to_string() });
                    text.push_str(d);
                }
                "response.output_item.done" if v["item"]["type"] == "function_call" => {
                    let it = &v["item"];
                    tool_calls.push(ToolCall {
                        id: it["call_id"].as_str().unwrap_or("").into(),
                        name: it["name"].as_str().unwrap_or("").into(),
                        arguments: parse_args(it["arguments"].as_str().unwrap_or("")),
                        meta: None,
                    });
                }
                "response.failed" | "error" => bail!("{}", failure(&v)),
                _ => {}
            }
            Ok(())
        })
        .await?;
        Ok(Response { text: Some(text).filter(|t| !t.is_empty()), tool_calls, reasoning: None })
    }

    fn has_web_search(&self) -> bool {
        true
    }

    async fn web_search(&self, query: &str) -> Result<String> {
        let input = vec![json!({"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": format!("{}{query}", super::SEARCH_PROMPT)}]})];
        let resp = self.post("", input, vec![json!({"type": "web_search"})]).await?;
        let mut text = String::new();
        let mut sources = Vec::new();
        sse(resp, |_, data| {
            let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(()) };
            match v["type"].as_str().unwrap_or("") {
                "response.output_text.delta" => text.push_str(v["delta"].as_str().unwrap_or("")),
                "response.output_item.done" => {
                    for c in v["item"]["content"].as_array().into_iter().flatten() {
                        for a in c["annotations"].as_array().into_iter().flatten() {
                            sources.push((
                                a["title"].as_str().unwrap_or("").to_string(),
                                a["url"].as_str().unwrap_or("").to_string(),
                            ));
                        }
                    }
                }
                "response.failed" | "error" => bail!("{}", failure(&v)),
                _ => {}
            }
            Ok(())
        })
        .await?;
        Ok(super::with_sources(text, &sources))
    }
}
