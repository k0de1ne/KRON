use anyhow::Result;
use proto::{LlmBackend, LlmConfig, Message, Response, Tool, ToolCall};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use std::process::Stdio;

pub struct External {
    command: Vec<String>,
}

impl External {
    pub fn new(cfg: &LlmConfig) -> Result<Self> {
        let raw = cfg.command.clone()
            .ok_or_else(|| anyhow::anyhow!(
                "provider = \"external\" requires [[llm]] command = \"path/to/script\""
            ))?;
        let parts: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
        if parts.is_empty() {
            anyhow::bail!("llm.command is empty");
        }
        Ok(External { command: parts })
    }
}

// ── Wire format ───────────────────────────────────────────────────────────────

fn build_request(system: &str, messages: &[Message], tools: &[Tool]) -> Value {
    let msgs: Vec<Value> = messages.iter().map(|m| match m {
        Message::User(t) => json!({"role": "user", "content": t}),
        Message::Assistant { text, tool_calls } => {
            let tcs: Vec<Value> = tool_calls.iter().map(|tc| json!({
                "id": tc.id, "name": tc.name, "arguments": tc.arguments
            })).collect();
            json!({"role": "assistant", "content": text, "tool_calls": tcs})
        }
        Message::ToolResult { tool_call_id, content } => {
            json!({"role": "tool", "tool_call_id": tool_call_id, "content": content})
        }
    }).collect();

    let ts: Vec<Value> = tools.iter().map(|t| json!({
        "name": t.name, "description": t.description, "parameters": t.parameters
    })).collect();

    json!({"system": system, "messages": msgs, "tools": ts})
}

#[derive(Deserialize)]
struct ExtResponse {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ExtCall>,
    #[serde(default = "default_done")]
    done: bool,
}

fn default_done() -> bool { true }

#[derive(Deserialize)]
struct ExtCall {
    #[serde(default)]
    id: String,
    name: String,
    #[serde(default)]
    arguments: Value,
}

// ── LlmBackend impl ───────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl LlmBackend for External {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<Response> {
        let payload = serde_json::to_string(&build_request(system, messages, tools))?;

        let mut child = Command::new(&self.command[0])
            .args(&self.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn '{}': {}", self.command[0], e))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(payload.as_bytes()).await?;
        }

        let out = child.wait_with_output().await?;
        if !out.status.success() {
            anyhow::bail!("external LLM exited with {}", out.status);
        }

        let ext: ExtResponse = serde_json::from_slice(&out.stdout)
            .map_err(|e| anyhow::anyhow!("external LLM returned invalid JSON: {e}\nraw: {}", String::from_utf8_lossy(&out.stdout)))?;

        let tool_calls = ext.tool_calls.into_iter().enumerate().map(|(i, tc)| {
            let id = if tc.id.is_empty() { format!("ext_{i}") } else { tc.id };
            ToolCall { id, name: tc.name, arguments: tc.arguments }
        }).collect();

        Ok(Response { text: ext.text, tool_calls, done: ext.done })
    }
}
