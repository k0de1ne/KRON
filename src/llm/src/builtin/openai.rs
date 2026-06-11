use anyhow::Result;
use futures::StreamExt;
use proto::{AgentEvent, EventSink, LlmBackend, LlmConfig, Message, Response, Tool, ToolCall};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub struct OpenAI {
    client: Client,
    api_key: Option<String>,
    model: String,
    base_url: String,
    max_tokens: u32,
}

impl OpenAI {
    pub fn new(cfg: &LlmConfig, api_key: Option<String>) -> Self {
        OpenAI {
            client: Client::new(),
            api_key,
            model: cfg.model.clone(),
            base_url: cfg.base_url.clone()
                .unwrap_or_else(|| "https://api.openai.com".to_string())
                .trim_end_matches('/')
                .to_string(),
            max_tokens: cfg.max_tokens.unwrap_or(2048),
        }
    }
}

// ── Request types ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Req {
    model: String,
    messages: Vec<OaiMsg>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OaiToolDef>,
    max_tokens: u32,
    stream: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct OaiMsg {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct OaiToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: OaiFn,
}

#[derive(Serialize, Deserialize, Clone)]
struct OaiFn {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct OaiToolDef {
    #[serde(rename = "type")]
    kind: String,
    function: OaiToolFn,
}

#[derive(Serialize)]
struct OaiToolFn {
    name: String,
    description: String,
    parameters: Value,
}

// ── Streaming response types ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Delta {
    content: Option<String>,
    tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Deserialize)]
struct DeltaToolCall {
    index: usize,
    id: Option<String>,
    function: Option<DeltaFn>,
}

#[derive(Deserialize)]
struct DeltaFn {
    name: Option<String>,
    arguments: Option<String>,
}

// ── Non-streaming response types (fallback) ───────────────────────────────────

#[derive(Deserialize)]
struct OaiResp {
    choices: Vec<OaiChoice>,
}

#[derive(Deserialize)]
struct OaiChoice {
    message: OaiMsg,
    finish_reason: Option<String>,
}

// ── Conversion ────────────────────────────────────────────────────────────────

fn to_oai(system: &str, messages: &[Message]) -> Vec<OaiMsg> {
    let mut out = vec![OaiMsg {
        role: "system".to_string(),
        content: Some(system.to_string()),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }];

    for m in messages {
        out.push(match m {
            Message::User(text) => OaiMsg {
                role: "user".to_string(),
                content: Some(text.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message::Assistant { text, tool_calls } => OaiMsg {
                role: "assistant".to_string(),
                content: text.clone(),
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls.iter().map(|tc| OaiToolCall {
                        id: tc.id.clone(),
                        kind: "function".to_string(),
                        function: OaiFn {
                            name: tc.name.clone(),
                            arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
                        },
                    }).collect())
                },
                tool_call_id: None,
                name: None,
            },
            Message::ToolResult { tool_call_id, content } => OaiMsg {
                role: "tool".to_string(),
                content: Some(content.clone()),
                tool_calls: None,
                tool_call_id: Some(tool_call_id.clone()),
                name: None,
            },
        });
    }
    out
}

// ── LlmBackend impl ───────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl LlmBackend for OpenAI {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<Response> {
        let oai_tools = make_tools(tools);
        let builder = self.build_request(to_oai(system, messages), oai_tools, false);

        let resp = builder.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM error {}: {}", status, body);
        }

        let oai: OaiResp = resp.json().await?;
        let choice = oai.choices.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("empty LLM response"))?;

        let finish = choice.finish_reason.as_deref().unwrap_or("stop");
        let msg = choice.message;

        let tool_calls = parse_tool_calls(msg.tool_calls.unwrap_or_default());

        Ok(Response {
            text: msg.content,
            tool_calls,
            done: matches!(finish, "stop" | "end_turn" | "length"),
        })
    }

    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
        sink: &dyn EventSink,
    ) -> Result<Response> {
        let oai_tools = make_tools(tools);
        let builder = self.build_request(to_oai(system, messages), oai_tools, true);

        let resp = builder.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM error {}: {}", status, body);
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();

        // Accumulate tool call fragments: index → (id, name, arguments)
        let mut tool_acc: Vec<(String, String, String)> = Vec::new();
        let mut full_text = String::new();
        let mut finish = String::from("stop");

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buf.push_str(&String::from_utf8_lossy(&bytes));

            // SSE lines end with \n; process complete lines
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim_end_matches('\r').to_string();
                buf.drain(..nl + 1);

                let Some(data) = line.strip_prefix("data: ") else { continue };
                if data == "[DONE]" { break; }

                let Ok(sc) = serde_json::from_str::<StreamChunk>(data) else { continue };
                let Some(choice) = sc.choices.into_iter().next() else { continue };

                if let Some(fr) = choice.finish_reason {
                    finish = fr;
                }

                if let Some(content) = choice.delta.content {
                    sink.emit(AgentEvent::Text { content: content.clone() }).await;
                    full_text.push_str(&content);
                }

                if let Some(delta_calls) = choice.delta.tool_calls {
                    for dc in delta_calls {
                        let idx = dc.index;
                        while tool_acc.len() <= idx {
                            tool_acc.push((String::new(), String::new(), String::new()));
                        }
                        if let Some(id) = dc.id { tool_acc[idx].0 = id; }
                        if let Some(f) = dc.function {
                            if let Some(n) = f.name { tool_acc[idx].1 = n; }
                            if let Some(a) = f.arguments { tool_acc[idx].2.push_str(&a); }
                        }
                    }
                }
            }
        }

        let tool_calls: Vec<ToolCall> = tool_acc.into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, args)| {
                let arguments = serde_json::from_str::<Value>(&args)
                    .unwrap_or(Value::String(args));
                ToolCall { id, name, arguments }
            })
            .collect();

        Ok(Response {
            text: if full_text.is_empty() { None } else { Some(full_text) },
            tool_calls,
            done: matches!(finish.as_str(), "stop" | "end_turn" | "length"),
        })
    }
}

fn make_tools(tools: &[Tool]) -> Vec<OaiToolDef> {
    tools.iter().map(|t| OaiToolDef {
        kind: "function".to_string(),
        function: OaiToolFn {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.parameters.clone(),
        },
    }).collect()
}

fn parse_tool_calls(raw: Vec<OaiToolCall>) -> Vec<ToolCall> {
    raw.into_iter().map(|tc| {
        let args = serde_json::from_str::<Value>(&tc.function.arguments)
            .unwrap_or(Value::String(tc.function.arguments));
        ToolCall { id: tc.id, name: tc.function.name, arguments: args }
    }).collect()
}

// Helper method (not part of trait, used internally)
impl OpenAI {
    fn build_request(&self, messages: Vec<OaiMsg>, tools: Vec<OaiToolDef>, stream: bool) -> reqwest::RequestBuilder {
        let req = Req {
            model: self.model.clone(),
            messages,
            tools,
            max_tokens: self.max_tokens,
            stream,
        };

        let mut builder = self.client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&req);

        if let Some(key) = &self.api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }
        builder
    }
}
