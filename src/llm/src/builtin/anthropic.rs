use anyhow::Result;
use futures::StreamExt;
use proto::{AgentEvent, EventSink, LlmBackend, LlmConfig, Message, Response, Tool, ToolCall};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub struct Anthropic {
    client: Client,
    api_key: String,
    model: String,
    max_tokens: u32,
}

impl Anthropic {
    pub fn new(cfg: &LlmConfig, api_key: Option<String>) -> Self {
        Anthropic {
            client: Client::new(),
            api_key: api_key
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .unwrap_or_default(),
            model: cfg.model.clone(),
            max_tokens: cfg.max_tokens.unwrap_or(4096),
        }
    }

    fn builder(&self, body: &Value, stream: bool) -> reqwest::RequestBuilder {
        let mut b = body.clone();
        if stream {
            b["stream"] = json!(true);
        }
        self.client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&b)
    }
}

// ── Message conversion ────────────────────────────────────────────────────────

fn to_anthropic(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        match &messages[i] {
            Message::User(text) => {
                out.push(json!({"role": "user", "content": text}));
                i += 1;
            }
            Message::Assistant { text, tool_calls } => {
                let mut content: Vec<Value> = Vec::new();
                if let Some(t) = text {
                    content.push(json!({"type": "text", "text": t}));
                }
                for tc in tool_calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments
                    }));
                }
                out.push(json!({"role": "assistant", "content": content}));
                i += 1;
            }
            Message::ToolResult { .. } => {
                // Group consecutive tool results into one user message
                let mut results: Vec<Value> = Vec::new();
                while i < messages.len() {
                    if let Message::ToolResult { tool_call_id, content } = &messages[i] {
                        results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": content
                        }));
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(json!({"role": "user", "content": results}));
            }
        }
    }
    out
}

fn make_tools(tools: &[Tool]) -> Vec<Value> {
    tools.iter().map(|t| json!({
        "name": t.name,
        "description": t.description,
        "input_schema": t.parameters
    })).collect()
}

fn build_body(model: &str, max_tokens: u32, system: &str, messages: &[Message], tools: &[Tool]) -> Value {
    json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": to_anthropic(messages),
        "tools": make_tools(tools)
    })
}

// ── Streaming response types ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct StreamEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    delta: Option<ContentDelta>,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    content_block: Option<ContentBlock>,
    #[serde(default)]
    message: Option<AnthMessage>,
}

#[derive(Deserialize)]
struct ContentDelta {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    partial_json: Option<String>,
    stop_reason: Option<String>,
}

#[derive(Deserialize, Clone)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct AnthMessage {
    stop_reason: Option<String>,
}

// ── Non-streaming response ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AnthResp {
    content: Vec<AnthContent>,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct AnthContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
}

// ── LlmBackend impl ───────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl LlmBackend for Anthropic {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<Response> {
        let body = build_body(&self.model, self.max_tokens, system, messages, tools);
        let resp = self.builder(&body, false).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic error {}: {}", status, text);
        }

        let ar: AnthResp = resp.json().await?;
        let stop = ar.stop_reason.as_deref().unwrap_or("end_turn");

        let mut text_out = String::new();
        let mut tool_calls = Vec::new();

        for block in ar.content {
            match block.kind.as_str() {
                "text" => {
                    if let Some(t) = block.text { text_out.push_str(&t); }
                }
                "tool_use" => {
                    tool_calls.push(ToolCall {
                        id: block.id.unwrap_or_default(),
                        name: block.name.unwrap_or_default(),
                        arguments: block.input.unwrap_or(Value::Object(Default::default())),
                    });
                }
                _ => {}
            }
        }

        Ok(Response {
            text: if text_out.is_empty() { None } else { Some(text_out) },
            tool_calls,
            done: matches!(stop, "end_turn" | "max_tokens"),
        })
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
        sink: &dyn EventSink,
    ) -> Result<Response> {
        let body = build_body(&self.model, self.max_tokens, system, messages, tools);
        let resp = self.builder(&body, true).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic error {}: {}", status, text);
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();

        let mut full_text = String::new();
        let mut stop_reason = String::from("end_turn");

        // Track content blocks by index: (id, name, accumulated_json)
        let mut blocks: Vec<ContentBlock> = Vec::new();
        let mut tool_json: std::collections::HashMap<usize, String> = Default::default();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buf.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim_end_matches('\r').to_string();
                buf.drain(..nl + 1);

                let Some(data) = line.strip_prefix("data: ") else { continue };
                let Ok(ev) = serde_json::from_str::<StreamEvent>(data) else { continue };

                match ev.kind.as_str() {
                    "content_block_start" => {
                        let idx = ev.index.unwrap_or(blocks.len());
                        while blocks.len() <= idx {
                            blocks.push(ContentBlock { kind: String::new(), id: None, name: None });
                        }
                        if let Some(cb) = ev.content_block {
                            blocks[idx] = cb;
                        }
                    }
                    "content_block_delta" => {
                        let idx = ev.index.unwrap_or(0);
                        if let Some(delta) = ev.delta {
                            let block_kind = blocks.get(idx).map(|b| b.kind.as_str()).unwrap_or("");
                            match block_kind {
                                "text" => {
                                    if let Some(t) = delta.text {
                                        sink.emit(AgentEvent::Text { content: t.clone() }).await;
                                        full_text.push_str(&t);
                                    }
                                }
                                "tool_use" => {
                                    if let Some(frag) = delta.partial_json {
                                        tool_json.entry(idx).or_default().push_str(&frag);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "message_delta" => {
                        if let Some(delta) = ev.delta {
                            if let Some(sr) = delta.stop_reason {
                                stop_reason = sr;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let tool_calls: Vec<ToolCall> = blocks.iter().enumerate()
            .filter(|(_, b)| b.kind == "tool_use")
            .map(|(idx, b)| {
                let raw = tool_json.get(&idx).cloned().unwrap_or_default();
                let arguments = serde_json::from_str::<Value>(&raw)
                    .unwrap_or(Value::Object(Default::default()));
                ToolCall {
                    id: b.id.clone().unwrap_or_default(),
                    name: b.name.clone().unwrap_or_default(),
                    arguments,
                }
            })
            .collect();

        Ok(Response {
            text: if full_text.is_empty() { None } else { Some(full_text) },
            tool_calls,
            done: matches!(stop_reason.as_str(), "end_turn" | "max_tokens"),
        })
    }
}
