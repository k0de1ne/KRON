//! OpenAI Responses API. Used with an API key (OpenAI, CLIProxyAPI, any
//! compatible gateway) or with a ChatGPT Plus/Pro subscription through the
//! Codex backend (kind = "codex", unofficial: requests look like the Codex CLI;
//! header values follow CLIProxyAPI).

use super::{Provider, Request, Response, api_key, check, http, openai::parse_args, sse, with_headers};
use crate::auth::Auth;
use crate::config::ProviderCfg;
use crate::tools::APPLY_PATCH_LARK;
use crate::types::{Event, Message, Sink, ToolCall, new_id};
use anyhow::{Result, bail};
use serde_json::{Value, json};

pub struct Responses {
    cfg: ProviderCfg,
    base: String,
    key: Option<String>,
    /// ChatGPT login (codex); None = API key.
    oauth: Option<Auth>,
    session: String,
}

impl Responses {
    pub fn with_key(p: &ProviderCfg) -> Self {
        Responses {
            cfg: p.clone(),
            base: base(p, "https://api.openai.com/v1"),
            key: api_key(p).or_else(|| std::env::var("OPENAI_API_KEY").ok()),
            oauth: None,
            session: new_id(),
        }
    }

    pub fn codex(p: &ProviderCfg, auth: Auth) -> Self {
        Responses {
            cfg: p.clone(),
            base: base(p, "https://chatgpt.com/backend-api/codex"),
            key: None,
            oauth: Some(auth),
            session: new_id(),
        }
    }

    async fn post(&self, system: &str, input: Vec<Value>, tools: Vec<Value>) -> Result<reqwest::Response> {
        let mut body = json!({
            "model": self.cfg.model,
            "instructions": system,
            "input": input,
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
        for attempt in 0..2 {
            let mut rb =
                http().post(format!("{}/responses", self.base)).header("accept", "text/event-stream").json(&body);
            match &self.oauth {
                Some(auth) => {
                    let creds = if attempt == 0 { auth.token().await? } else { auth.refresh().await? };
                    rb = rb
                        .bearer_auth(&creds.access_token)
                        .header("originator", "codex-tui")
                        .header(
                            "user-agent",
                            format!("codex-tui/0.154.0 ({}; {})", std::env::consts::OS, std::env::consts::ARCH),
                        )
                        .header("session_id", &self.session);
                    if let Some(acc) = &creds.account_id {
                        rb = rb.header("chatgpt-account-id", acc);
                    }
                }
                None => {
                    if let Some(k) = &self.key {
                        rb = rb.bearer_auth(k);
                    }
                }
            }
            let resp = with_headers(rb, &self.cfg).send().await?;
            if resp.status() == 401 && self.oauth.is_some() && attempt == 0 {
                continue;
            }
            return check(resp).await;
        }
        unreachable!()
    }
}

fn base(p: &ProviderCfg, default: &str) -> String {
    p.base_url.clone().unwrap_or_else(|| default.into()).trim_end_matches('/').to_string()
}

fn is_custom(tc: &ToolCall) -> bool {
    tc.meta.as_ref().is_some_and(|m| m["custom"] == true)
}

fn to_input(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut custom_ids = std::collections::HashSet::new();
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
                    if is_custom(tc) {
                        custom_ids.insert(tc.id.clone());
                        let input = tc.arguments["input"].as_str().unwrap_or_default();
                        out.push(
                            json!({"type": "custom_tool_call", "call_id": tc.id, "name": tc.name, "input": input}),
                        );
                    } else {
                        out.push(json!({"type": "function_call", "call_id": tc.id, "name": tc.name,
                            "arguments": tc.arguments.to_string()}));
                    }
                }
            }
            Message::Tool { call_id, content, .. } => {
                let kind =
                    if custom_ids.contains(call_id) { "custom_tool_call_output" } else { "function_call_output" };
                out.push(json!({"type": kind, "call_id": call_id, "output": content}))
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
impl Provider for Responses {
    async fn complete(&self, req: Request<'_>, sink: &dyn Sink) -> Result<Response> {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                if t.name == "apply_patch" {
                    // GPT models are trained on Codex's freeform apply_patch.
                    json!({"type": "custom", "name": "apply_patch",
                        "description": "Use the `apply_patch` tool to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.",
                        "format": {"type": "grammar", "syntax": "lark", "definition": APPLY_PATCH_LARK}})
                } else {
                    json!({"type": "function", "name": t.name, "description": t.description,
                        "strict": false, "parameters": t.parameters})
                }
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
                "response.output_item.done" => {
                    let it = &v["item"];
                    let call = |arguments, meta| ToolCall {
                        id: it["call_id"].as_str().unwrap_or("").into(),
                        name: it["name"].as_str().unwrap_or("").into(),
                        arguments,
                        meta,
                    };
                    match it["type"].as_str() {
                        Some("function_call") => {
                            tool_calls.push(call(parse_args(it["arguments"].as_str().unwrap_or("")), None))
                        }
                        Some("custom_tool_call") => tool_calls.push(call(
                            json!({"input": it["input"].as_str().unwrap_or("")}),
                            Some(json!({"custom": true})),
                        )),
                        _ => {}
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_calls_round_trip() {
        let msgs = vec![
            Message::user("edit"),
            Message::Assistant {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "apply_patch".into(),
                    arguments: json!({"input": "*** Begin Patch"}),
                    meta: Some(json!({"custom": true})),
                }],
                reasoning: None,
            },
            Message::Tool { call_id: "c1".into(), name: "apply_patch".into(), content: "ok".into() },
        ];
        let input = to_input(&msgs);
        assert_eq!(input[1]["type"], "custom_tool_call");
        assert_eq!(input[1]["input"], "*** Begin Patch");
        assert_eq!(input[2]["type"], "custom_tool_call_output");
    }
}
