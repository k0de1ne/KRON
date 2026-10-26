use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool definition shown to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    /// Provider-specific data that must be echoed back (e.g. Gemini thought signatures).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        content: String,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
        /// Reasoning text some APIs (DeepSeek) want back during a tool loop.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },
    Tool {
        call_id: String,
        name: String,
        content: String,
    },
}

impl Message {
    pub fn user(s: impl Into<String>) -> Self {
        Message::User { content: s.into() }
    }
    pub fn assistant(s: impl Into<String>) -> Self {
        Message::Assistant { text: Some(s.into()), tool_calls: vec![], reasoning: None }
    }
}

/// Streamed to CLIs and the web UI while a run is in progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Text { content: String },
    ToolStart { name: String, args: String },
    ToolEnd { name: String, output: String },
    Error { message: String },
    Done,
}

pub trait Sink: Send + Sync {
    fn emit(&self, event: Event);
}

/// CLI sink: answer text to stdout, tool activity to stderr.
pub struct Stdout;

impl Sink for Stdout {
    fn emit(&self, event: Event) {
        use std::io::Write;
        match event {
            Event::Text { content } => {
                print!("{content}");
                std::io::stdout().flush().ok();
            }
            Event::ToolStart { name, args } => eprintln!("\n· {name} {}", preview(&args, 300)),
            Event::ToolEnd { output, .. } => eprintln!("  {}", preview(&output, 300).replace('\n', "\n  ")),
            Event::Error { message } => eprintln!("\nerror: {message}"),
            Event::Done => println!(),
        }
    }
}

pub struct NullSink;

impl Sink for NullSink {
    fn emit(&self, _: Event) {}
}

pub fn preview(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn new_id() -> String {
    use rand::Rng;
    let n: u64 = rand::thread_rng().gen();
    format!("{:x}{:016x}", now_ms(), n)
}
