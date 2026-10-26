//! Google subscriptions through the Code Assist API (cloudcode-pa), the same
//! backend the Gemini CLI (kind = "gemini") and Antigravity (kind =
//! "antigravity") use. Unofficial.

use super::{Provider, Request, Response, check, http, sse, with_headers};
use crate::auth::Auth;
use crate::config::ProviderCfg;
use crate::types::{Event, Message, Sink, ToolCall, new_id};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

pub struct Gemini {
    cfg: ProviderCfg,
    auth: Auth,
    base: String,
    antigravity: bool,
    session: String,
}

impl Gemini {
    pub fn new(cfg: &ProviderCfg, auth: Auth) -> Self {
        let antigravity = cfg.kind == "antigravity";
        let default = if antigravity {
            "https://daily-cloudcode-pa.googleapis.com"
        } else {
            "https://cloudcode-pa.googleapis.com"
        };
        Gemini {
            cfg: cfg.clone(),
            auth,
            base: cfg.base_url.clone().unwrap_or_else(|| default.into()).trim_end_matches('/').to_string(),
            antigravity,
            session: new_id(),
        }
    }

    async fn call(&self, method: &str, body: &Value) -> Result<reqwest::Response> {
        for attempt in 0..2 {
            let creds = if attempt == 0 { self.auth.token().await? } else { self.auth.refresh().await? };
            let mut rb =
                http().post(format!("{}/v1internal:{method}", self.base)).bearer_auth(creds.access_token).json(body);
            // Client identities as sent by Antigravity (via CLIProxyAPI) and Gemini CLI.
            let ua = if self.antigravity {
                "antigravity/hub/2.24.0 (aidev_client; os_type=darwin; arch=arm64)".to_string()
            } else {
                format!(
                    "GeminiCLI/0.40.0/{} ({}; {}; terminal)",
                    self.cfg.model,
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )
            };
            rb = rb.header("user-agent", ua);
            let resp = with_headers(rb, &self.cfg).send().await?;
            if resp.status() == 401 && attempt == 0 {
                continue;
            }
            return check(resp).await;
        }
        unreachable!()
    }

    /// The Cloud project requests are billed to: configured, cached, or discovered.
    async fn project(&self) -> Result<String> {
        if let Some(p) = self.cfg.project.clone().or_else(|| std::env::var("GOOGLE_CLOUD_PROJECT").ok()) {
            return Ok(p);
        }
        if let Some(p) = self.auth.token().await?.project {
            return Ok(p);
        }
        let meta = json!({"ideType": "IDE_UNSPECIFIED", "platform": "PLATFORM_UNSPECIFIED", "pluginType": "GEMINI"});
        let v: Value = self.call("loadCodeAssist", &json!({"metadata": meta})).await?.json().await?;
        let mut project = project_id(&v["cloudaicompanionProject"]);
        if project.is_none() {
            let tier = v["allowedTiers"]
                .as_array()
                .and_then(|t| t.iter().find(|t| t["isDefault"] == true))
                .and_then(|t| t["id"].as_str())
                .unwrap_or("free-tier")
                .to_string();
            for _ in 0..12 {
                let op: Value =
                    self.call("onboardUser", &json!({"tierId": tier, "metadata": meta})).await?.json().await?;
                if op["done"] == true {
                    project = project_id(&op["response"]["cloudaicompanionProject"]);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
        let project = project.context("could not determine a Code Assist project; set `project` for this provider")?;
        self.auth.set_project(&project).await?;
        Ok(project)
    }

    async fn envelope(&self, request: Value) -> Result<Value> {
        let mut body = json!({"model": self.cfg.model, "project": self.project().await?, "request": request});
        if self.antigravity {
            body["userAgent"] = json!("antigravity");
            body["requestType"] = json!("agent");
            body["requestId"] = json!(format!("agent-{}", new_id()));
            body["request"]["sessionId"] = json!(self.session);
        } else {
            body["user_prompt_id"] = json!(new_id());
            body["request"]["session_id"] = json!(self.session);
        }
        Ok(body)
    }
}

fn project_id(v: &Value) -> Option<String> {
    v.as_str().or_else(|| v["id"].as_str()).filter(|s| !s.is_empty()).map(String::from)
}

fn to_contents(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        let (role, parts) = match m {
            Message::User { content } => ("user", vec![json!({"text": content})]),
            Message::Assistant { text, tool_calls, .. } => {
                let mut parts = Vec::new();
                if let Some(t) = text.as_ref().filter(|t| !t.is_empty()) {
                    parts.push(json!({"text": t}));
                }
                for tc in tool_calls {
                    let mut p = json!({"functionCall": {"name": tc.name, "args": tc.arguments}});
                    if let Some(sig) = &tc.meta {
                        p["thoughtSignature"] = sig.clone();
                    }
                    parts.push(p);
                }
                ("model", parts)
            }
            Message::Tool { name, content, .. } => {
                ("user", vec![json!({"functionResponse": {"name": name, "response": {"output": content}}})])
            }
        };
        if parts.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last["role"] == role => last["parts"].as_array_mut().unwrap().extend(parts),
            _ => out.push(json!({"role": role, "parts": parts})),
        }
    }
    out
}

/// Gemini accepts only an OpenAPI subset of JSON schema.
fn clean_schema(v: &Value) -> Value {
    const KEEP: &[&str] = &["type", "description", "enum", "required", "format", "nullable", "minimum", "maximum"];
    let Some(obj) = v.as_object() else { return v.clone() };
    let mut out = Map::new();
    for (k, val) in obj {
        if KEEP.contains(&k.as_str()) {
            out.insert(k.clone(), val.clone());
        } else if k == "properties" {
            let props = val.as_object().map(|p| p.iter().map(|(n, s)| (n.clone(), clean_schema(s))).collect());
            out.insert(k.clone(), Value::Object(props.unwrap_or_default()));
        } else if k == "items" {
            out.insert(k.clone(), clean_schema(val));
        }
    }
    Value::Object(out)
}

#[async_trait::async_trait]
impl Provider for Gemini {
    async fn complete(&self, req: Request<'_>, sink: &dyn Sink) -> Result<Response> {
        let mut request = json!({"contents": to_contents(req.messages)});
        if !req.system.is_empty() {
            request["systemInstruction"] = json!({"role": "user", "parts": [{"text": req.system}]});
        }
        if !req.tools.is_empty() {
            let decls: Vec<Value> = req
                .tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "parameters": clean_schema(&t.parameters)}))
                .collect();
            request["tools"] = json!([{"functionDeclarations": decls}]);
        }
        if let Some(n) = self.cfg.max_tokens {
            request["generationConfig"] = json!({"maxOutputTokens": n});
        }
        let body = self.envelope(request).await?;
        let resp = self.call("streamGenerateContent?alt=sse", &body).await?;

        let mut text = String::new();
        let mut thoughts = String::new();
        let mut tool_calls = Vec::new();
        sse(resp, |_, data| {
            let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(()) };
            let r = if v["response"].is_object() { &v["response"] } else { &v };
            if let Some(e) = r.get("error") {
                bail!("{e}");
            }
            for part in r["candidates"][0]["content"]["parts"].as_array().into_iter().flatten() {
                if let Some(fc) = part.get("functionCall") {
                    tool_calls.push(ToolCall {
                        id: fc["id"].as_str().map(String::from).unwrap_or_else(|| format!("call_{}", tool_calls.len())),
                        name: fc["name"].as_str().unwrap_or("").into(),
                        arguments: fc.get("args").cloned().unwrap_or(json!({})),
                        meta: part.get("thoughtSignature").cloned(),
                    });
                } else if let Some(t) = part["text"].as_str() {
                    if part["thought"] == true {
                        sink.emit(Event::Reasoning { content: t.to_string() });
                        thoughts.push_str(t);
                    } else {
                        sink.emit(Event::Text { content: t.to_string() });
                        text.push_str(t);
                    }
                }
            }
            Ok(())
        })
        .await?;
        let reasoning = Some(thoughts).filter(|t| !t.is_empty());
        Ok(Response { text: Some(text).filter(|t| !t.is_empty()), tool_calls, reasoning })
    }

    fn has_web_search(&self) -> bool {
        true
    }

    async fn web_search(&self, query: &str) -> Result<String> {
        let request = json!({
            "contents": [{"role": "user", "parts": [{"text": format!("{}{query}", super::SEARCH_PROMPT)}]}],
            "tools": [{"googleSearch": {}}],
        });
        let body = self.envelope(request).await?;
        let v: Value = self.call("generateContent", &body).await?.json().await?;
        let r = if v["response"].is_object() { &v["response"] } else { &v };
        let c = &r["candidates"][0];
        let text: String = c["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["thought"] != true)
            .filter_map(|p| p["text"].as_str())
            .collect();
        let sources: Vec<(String, String)> = c["groundingMetadata"]["groundingChunks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|g| (g["web"]["title"].as_str().unwrap_or("").into(), g["web"]["uri"].as_str().unwrap_or("").into()))
            .collect();
        Ok(super::with_sources(text, &sources))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_cleaned() {
        let s = json!({"type": "object", "additionalProperties": false, "$schema": "x",
            "properties": {"a": {"type": "array", "items": {"type": "string", "default": "q"}}}});
        assert_eq!(
            clean_schema(&s),
            json!({"type": "object", "properties": {"a": {"type": "array", "items": {"type": "string"}}}})
        );
    }

    #[test]
    fn tool_results_grouped() {
        let msgs = vec![
            Message::user("hi"),
            Message::Assistant {
                text: None,
                tool_calls: vec![
                    ToolCall { id: "1".into(), name: "a".into(), arguments: json!({}), meta: Some(json!("sig")) },
                    ToolCall { id: "2".into(), name: "b".into(), arguments: json!({}), meta: None },
                ],
                reasoning: None,
            },
            Message::Tool { call_id: "1".into(), name: "a".into(), content: "x".into() },
            Message::Tool { call_id: "2".into(), name: "b".into(), content: "y".into() },
        ];
        let c = to_contents(&msgs);
        assert_eq!(c.len(), 3);
        assert_eq!(c[1]["parts"][0]["thoughtSignature"], "sig");
        assert_eq!(c[2]["parts"].as_array().unwrap().len(), 2);
    }
}
