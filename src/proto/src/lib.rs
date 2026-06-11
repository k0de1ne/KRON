use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub llm: LlmConfig,
    pub agent: AgentConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
    /// Shell command for provider = "external". Receives JSON on stdin, returns JSON on stdout.
    pub command: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    pub tool_paths: Vec<String>,
    pub hooks_dir: String,
    pub context_file: String,
    #[serde(default = "default_threads_dir")]
    pub threads_dir: String,
}

fn default_threads_dir() -> String { "./threads".to_string() }

impl Default for Config {
    fn default() -> Self {
        Config {
            llm: LlmConfig {
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
                api_key: None,
                base_url: None,
                max_tokens: Some(4096),
                command: None,
            },
            agent: AgentConfig {
                tool_paths: vec!["./extensions/tools".to_string()],
                hooks_dir: "./extensions/hooks".to_string(),
                context_file: "./system.md".to_string(),
                threads_dir: "./threads".to_string(),
            },
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let cfg_path = match path {
            Some(p) => p.to_path_buf(),
            None => PathBuf::from("config.toml"),
        };

        if cfg_path.exists() {
            let text = std::fs::read_to_string(&cfg_path)
                .with_context(|| format!("reading {:?}", cfg_path))?;
            toml::from_str(&text).with_context(|| "parsing config")
        } else {
            Ok(Config::default())
        }
    }

    pub fn resolve(&self, p: &str) -> PathBuf {
        if p.starts_with('~') {
            home_dir().join(p.trim_start_matches("~/").trim_start_matches("~\\"))
        } else {
            PathBuf::from(p)
        }
    }

    pub fn tool_paths(&self) -> Vec<PathBuf> {
        self.agent.tool_paths.iter().map(|p| self.resolve(p)).collect()
    }

    pub fn threads_dir(&self) -> PathBuf {
        self.resolve(&self.agent.threads_dir)
    }

    pub fn hooks_dir(&self) -> PathBuf {
        self.resolve(&self.agent.hooks_dir)
    }

    pub fn context_file(&self) -> PathBuf {
        self.resolve(&self.agent.context_file)
    }

    pub fn api_key(&self) -> Option<String> {
        self.llm.api_key.clone().or_else(|| std::env::var("OPENAI_API_KEY").ok())
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

// ── LLM types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    Assistant { text: Option<String>, tool_calls: Vec<ToolCall> },
    ToolResult { tool_call_id: String, content: String },
}

#[derive(Debug)]
pub struct Response {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub done: bool,
}

// ── Agent events ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Text { content: String },
    ToolStart { name: String, args: String },
    ToolEnd { name: String, output: String },
    Done,
}

// ── EventSink ─────────────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait EventSink: Send + Sync {
    async fn emit(&self, event: AgentEvent);
}

/// CLI sink: text → stdout, tool events → stderr
pub struct StdoutSink;

#[async_trait::async_trait]
impl EventSink for StdoutSink {
    async fn emit(&self, event: AgentEvent) {
        match event {
            AgentEvent::Text { content } => print!("{}", content),
            AgentEvent::ToolStart { name, args } => eprintln!("\n[tool: {}] {}", name, args),
            AgentEvent::ToolEnd { name, output } => {
                let preview: String = output.chars().take(200).collect();
                eprintln!("[result: {}] {}", name, preview);
            }
            AgentEvent::Done => {}
        }
    }
}

// ── LLM backend trait ─────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait LlmBackend: Send + Sync {
    /// Non-streaming completion (always available).
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<Response>;

    /// Streaming completion — emits Text chunks to sink as they arrive.
    /// Default falls back to complete() and emits the full text at once.
    async fn complete_streaming(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
        sink: &dyn EventSink,
    ) -> Result<Response> {
        let resp = self.complete(system, messages, tools).await?;
        if let Some(ref text) = resp.text {
            sink.emit(AgentEvent::Text { content: text.clone() }).await;
        }
        Ok(resp)
    }
}

// ── Tool manifest ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 { 30 }

impl ToolManifest {
    pub fn minimal(name: &str) -> Self {
        ToolManifest {
            name: name.to_string(),
            description: format!("Run the {} command. Input is a JSON object.", name),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "args": { "type": "string", "description": "Arguments to pass" }
                }
            }),
            timeout_secs: 30,
        }
    }
}
