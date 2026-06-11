use anyhow::Result;
use proto::{Config, EventSink, LlmBackend, Message, ToolCall};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Session {
    pub key: String,
    pub config: Config,
    messages: Vec<Message>,
    dir: PathBuf,
}

impl Session {
    /// Open (or create) a session by key.
    /// `config_override` — if Some, replaces the session's stored config wholesale.
    pub fn open(key: &str, config_override: Option<Config>, dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;

        let messages     = load_messages(dir, key)?;
        let stored_config = load_config(dir, key)?;
        let config       = config_override.unwrap_or(stored_config);

        Ok(Session { key: key.to_string(), config, messages, dir: dir.to_path_buf() })
    }

    /// Run one task, append new messages to history, persist session.
    /// Returns the new messages added during this run.
    pub async fn run(
        &mut self,
        task: String,
        backend: Arc<dyn LlmBackend>,
        sink: Arc<dyn EventSink>,
    ) -> Result<Vec<Message>> {
        let new_messages = agent::run(
            task,
            self.messages.clone(),
            self.config.clone(),
            backend,
            sink,
        ).await?;

        append_messages(&self.dir, &self.key, &new_messages)?;
        save_config(&self.dir, &self.key, &self.config)?;

        self.messages.extend_from_slice(&new_messages);
        Ok(new_messages)
    }

    pub fn messages(&self) -> &[Message] { &self.messages }
    pub fn message_count(&self) -> usize { self.messages.len() }
}

// ── Persistence ───────────────────────────────────────────────────────────────

fn messages_path(dir: &Path, key: &str) -> PathBuf { dir.join(format!("{key}.jsonl")) }
fn config_path(dir: &Path, key: &str)   -> PathBuf { dir.join(format!("{key}.toml"))  }

fn load_messages(dir: &Path, key: &str) -> Result<Vec<Message>> {
    let path = messages_path(dir, key);
    if !path.exists() { return Ok(Vec::new()); }

    let content = std::fs::read_to_string(&path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(msg) = json_to_message(&v) {
                out.push(msg);
            }
        }
    }
    Ok(out)
}

fn load_config(dir: &Path, key: &str) -> Result<Config> {
    let path = config_path(dir, key);
    if !path.exists() { return Ok(Config::default()); }
    let text = std::fs::read_to_string(&path)?;
    Ok(toml::from_str(&text)?)
}

fn append_messages(dir: &Path, key: &str, messages: &[Message]) -> Result<()> {
    let path = messages_path(dir, key);
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    for msg in messages {
        writeln!(file, "{}", serde_json::to_string(&message_to_json(msg))?)?;
    }
    Ok(())
}

fn save_config(dir: &Path, key: &str, config: &Config) -> Result<()> {
    std::fs::write(config_path(dir, key), toml::to_string(config)?)?;
    Ok(())
}

// ── Message serialisation ─────────────────────────────────────────────────────

fn message_to_json(msg: &Message) -> Value {
    match msg {
        Message::User(content) => json!({"role": "user", "content": content}),
        Message::Assistant { text, tool_calls } => json!({
            "role": "assistant",
            "text": text,
            "tool_calls": tool_calls.iter().map(|tc| json!({
                "id": tc.id, "name": tc.name, "arguments": tc.arguments
            })).collect::<Vec<_>>()
        }),
        Message::ToolResult { tool_call_id, content } => json!({
            "role": "tool_result",
            "tool_call_id": tool_call_id,
            "content": content
        }),
    }
}

fn json_to_message(v: &Value) -> Option<Message> {
    match v["role"].as_str()? {
        "user" => Some(Message::User(v["content"].as_str()?.to_string())),
        "assistant" => Some(Message::Assistant {
            text: v.get("text").and_then(|t| t.as_str()).map(String::from),
            tool_calls: v["tool_calls"].as_array()
                .map(|arr| arr.iter().filter_map(|tc| Some(ToolCall {
                    id:        tc["id"].as_str()?.to_string(),
                    name:      tc["name"].as_str()?.to_string(),
                    arguments: tc["arguments"].clone(),
                })).collect())
                .unwrap_or_default(),
        }),
        "tool_result" => Some(Message::ToolResult {
            tool_call_id: v["tool_call_id"].as_str()?.to_string(),
            content:      v["content"].as_str()?.to_string(),
        }),
        _ => None,
    }
}
