//! OpenAI-compatible chat completions: OpenAI, DeepSeek, OpenRouter, llama.cpp, Ollama...

use super::{Provider, Request, Response, api_key, check, http, sse, with_headers};
use crate::config::ProviderCfg;
use crate::types::{Event, Message, Sink, ToolCall};
use anyhow::Result;
use serde_json::{Value, json};

pub struct OpenAi {
    cfg: ProviderCfg,
    base: String,
    key: Option<String>,
}

impl OpenAi {
    pub fn new(cfg: &ProviderCfg) -> Self {
        let base = cfg
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1".into())
            .trim_end_matches('/')
            .to_string();
        OpenAi { cfg: cfg.clone(), base, key: api_key(cfg) }
    }
}

pub fn to_messages(system: &str, messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    if !system.is_empty() {
        out.push(json!({"role": "system", "content": system}));
    }
    // Reasoning is only sent back for the turn in progress (DeepSeek thinking + tools).
    let last_user = messages.iter().rposition(|m| matches!(m, Message::User { .. })).unwrap_or(0);
    for (i, m) in messages.iter().enumerate() {
        out.push(match m {
            Message::User { content } => json!({"role": "user", "content": content}),
            Message::Assistant { text, tool_calls, reasoning } => {
                let mut v = json!({"role": "assistant", "content": text.clone().unwrap_or_default()});
                if !tool_calls.is_empty() {
                    v["tool_calls"] = tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {"name": tc.name, "arguments": tc.arguments.to_string()},
                            })
                        })
                        .collect();
                }
                if let (Some(r), true) = (reasoning, i > last_user) {
                    v["reasoning_content"] = json!(r);
                }
                v
            }
            Message::Tool { call_id, content, .. } => {
                json!({"role": "tool", "tool_call_id": call_id, "content": content})
            }
        });
    }
    out
}

#[async_trait::async_trait]
impl Provider for OpenAi {
    async fn complete(&self, req: Request<'_>, sink: &dyn Sink) -> Result<Response> {
        let mut body = json!({
            "model": self.cfg.model,
            "messages": to_messages(req.system, req.messages),
            "stream": true,
        });
        if let Some(n) = self.cfg.max_tokens {
            body["max_tokens"] = json!(n);
        }
        if !req.tools.is_empty() {
            body["tools"] = req
                .tools
                .iter()
                .map(|t| {
                    json!({"type": "function", "function": {
                        "name": t.name, "description": t.description, "parameters": t.parameters}})
                })
                .collect();
        }
        let mut rb = http().post(format!("{}/chat/completions", self.base)).json(&body);
        if let Some(k) = &self.key {
            rb = rb.bearer_auth(k);
        }
        let resp = check(with_headers(rb, &self.cfg).send().await?).await?;

        let mut text = String::new();
        let mut reasoning = String::new();
        // index -> (id, name, arguments)
        let mut calls: Vec<(String, String, String)> = Vec::new();
        sse(resp, |_, data| {
            if data == "[DONE]" {
                return Ok(());
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(()) };
            if let Some(err) = v.get("error") {
                anyhow::bail!("{err}");
            }
            let delta = &v["choices"][0]["delta"];
            if let Some(t) = delta["content"].as_str() {
                sink.emit(Event::Text { content: t.to_string() });
                text.push_str(t);
            }
            if let Some(t) = delta["reasoning_content"].as_str() {
                sink.emit(Event::Reasoning { content: t.to_string() });
                reasoning.push_str(t);
            }
            for dc in delta["tool_calls"].as_array().into_iter().flatten() {
                let idx = dc["index"].as_u64().unwrap_or(calls.len() as u64) as usize;
                while calls.len() <= idx {
                    calls.push(Default::default());
                }
                if let Some(id) = dc["id"].as_str() {
                    calls[idx].0 = id.to_string();
                }
                if let Some(n) = dc["function"]["name"].as_str() {
                    calls[idx].1.push_str(n);
                }
                if let Some(a) = dc["function"]["arguments"].as_str() {
                    calls[idx].2.push_str(a);
                }
            }
            Ok(())
        })
        .await?;

        let tool_calls = calls
            .into_iter()
            .filter(|c| !c.1.is_empty())
            .enumerate()
            .map(|(i, (id, name, args))| ToolCall {
                id: if id.is_empty() { format!("call_{i}") } else { id },
                name,
                arguments: parse_args(&args),
                meta: None,
            })
            .collect();
        Ok(Response {
            text: Some(text).filter(|t| !t.is_empty()),
            tool_calls,
            reasoning: Some(reasoning).filter(|r| !r.is_empty()),
        })
    }
}

pub fn parse_args(s: &str) -> Value {
    if s.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}
