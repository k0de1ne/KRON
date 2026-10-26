//! The agent loop: model turn → tool calls (in parallel) → results → repeat.

use crate::config::Config;
use crate::llm::{self, Provider, Request};
use crate::store::repair;
use crate::tools::{self, AGENT_SIDE};
use crate::types::{Event, Message, NullSink, Sink, Tool, ToolCall, preview};
use anyhow::Result;
use futures::future::join_all;
use serde_json::Value;
use std::sync::Arc;

/// Where tools run: this process, or a node connected to the hub.
#[async_trait::async_trait]
pub trait Executor: Send + Sync {
    async fn run_tool(&self, name: &str, args: &Value) -> String;
    /// Script tools available there.
    fn external_tools(&self) -> Vec<Tool>;
    /// OS, working directory and project instructions (AGENTS.md).
    fn environment(&self) -> String;
    /// One turn of a browser-driven web chat. Returns (answer, conversation url).
    async fn web_chat(&self, site: &str, prompt: &str, url: Option<&str>) -> Result<(String, String)>;
}

pub struct Local {
    pub cfg: Arc<Config>,
    pub ctx: tools::Ctx,
}

impl Local {
    pub fn new(cfg: &Arc<Config>, cwd: std::path::PathBuf) -> Self {
        Local { cfg: cfg.clone(), ctx: tools::Ctx { cwd, ext_dirs: vec![cfg.path("tools")] } }
    }
}

#[async_trait::async_trait]
impl Executor for Local {
    async fn run_tool(&self, name: &str, args: &Value) -> String {
        tools::run(&self.ctx, name, args).await
    }
    fn external_tools(&self) -> Vec<Tool> {
        tools::external(&self.ctx.ext_dirs)
    }
    fn environment(&self) -> String {
        environment(&self.cfg.node_name(), &self.ctx.cwd)
    }
    async fn web_chat(&self, site: &str, prompt: &str, url: Option<&str>) -> Result<(String, String)> {
        crate::browser::web_chat(&self.cfg, site, prompt, url).await
    }
}

pub fn environment(node: &str, cwd: &std::path::Path) -> String {
    let mut s = format!(
        "<environment>\nmachine: {node}\nos: {} ({})\ncwd: {}\nshell: {}\n</environment>",
        std::env::consts::OS,
        std::env::consts::ARCH,
        cwd.display(),
        if cfg!(windows) { "powershell" } else { "sh" },
    );
    for name in ["AGENTS.md", "CLAUDE.md", "GEMINI.md"] {
        if let Ok(text) = std::fs::read_to_string(cwd.join(name)) {
            s.push_str(&format!(
                "\n\n<project_instructions file=\"{name}\">\n{}\n</project_instructions>",
                text.trim()
            ));
            break;
        }
    }
    s
}

/// The Codex CLI system prompt (openai/codex, Apache-2.0), lightly adapted.
const BASE_PROMPT: &str = include_str!("prompts/codex.md");

pub struct Agent {
    cfg: Arc<Config>,
    pub agent: Option<String>,
    provider: Arc<dyn Provider>,
    system: String,
    allowed: Vec<String>,
    exec: Arc<dyn Executor>,
    depth: usize,
}

impl Agent {
    pub fn new(
        cfg: &Arc<Config>,
        agent: Option<&str>,
        provider: Option<&str>,
        model: Option<&str>,
        exec: Arc<dyn Executor>,
    ) -> Result<Agent> {
        let a = match agent {
            Some(name) => cfg.agents.get(name).cloned().ok_or_else(|| anyhow::anyhow!("unknown agent '{name}'"))?,
            None => Default::default(),
        };
        let provider_name = match provider.map(String::from).or(a.provider.clone()) {
            Some(p) => p,
            None => cfg.default_provider()?,
        };
        let provider = llm::build(cfg, &provider_name, model.or(a.model.as_deref()))?;
        let mut system = a.system.clone().unwrap_or_default();
        if let Some(f) = &a.system_file {
            let path = if std::path::Path::new(f).is_absolute() { f.into() } else { cfg.home.join(f) };
            system = format!("{system}\n\n{}", std::fs::read_to_string(path)?).trim().to_string();
        }
        Ok(Agent {
            cfg: cfg.clone(),
            agent: agent.map(String::from),
            provider,
            system,
            allowed: a.tools,
            exec,
            depth: 0,
        })
    }

    fn tools(&self) -> Vec<Tool> {
        let mut all = tools::builtin();
        if self.depth > 0 || self.cfg.agents.is_empty() {
            all.retain(|t| t.name != "spawn_agent");
        }
        if !self.provider.has_web_search() && self.cfg.search_provider().is_none() {
            all.retain(|t| t.name != "web_search");
        }
        for ext in self.exec.external_tools() {
            all.retain(|t| t.name != ext.name);
            all.push(ext);
        }
        if !self.allowed.is_empty() {
            all.retain(|t| self.allowed.contains(&t.name));
        }
        all
    }

    fn system_prompt(&self, with_tools: bool) -> String {
        if !with_tools {
            return self.system.clone();
        }
        let mut s = format!("{BASE_PROMPT}\n\n{}", self.exec.environment());
        if self.depth == 0 && !self.cfg.agents.is_empty() {
            s.push_str("\n\nAgents available to spawn_agent:");
            for (name, a) in &self.cfg.agents {
                let about =
                    a.system.as_deref().map(|t| preview(t.lines().next().unwrap_or(""), 120)).unwrap_or_default();
                s.push_str(&format!("\n- {name}: {about}"));
            }
        }
        if !self.system.is_empty() {
            s.push_str(&format!("\n\n{}", self.system));
        }
        s
    }

    /// Plain question/answer: one model call, no tools.
    pub async fn chat(&self, history: Vec<Message>, sink: &dyn Sink) -> Result<String> {
        let system = self.system_prompt(false);
        let messages = repair(history);
        let resp = self.provider.complete(Request { system: &system, messages: &messages, tools: &[] }, sink).await?;
        Ok(resp.text.unwrap_or_default())
    }

    /// Run until the model stops calling tools. `history` ends with the new user
    /// message; every produced message goes through `save` as soon as it exists.
    pub async fn run(
        &self,
        history: Vec<Message>,
        sink: &dyn Sink,
        save: &(dyn Fn(&Message) + Send + Sync),
    ) -> Result<String> {
        let tools = self.tools();
        let system = self.system_prompt(true);
        let mut messages = repair(history);
        loop {
            let resp =
                self.provider.complete(Request { system: &system, messages: &messages, tools: &tools }, sink).await?;
            let msg = Message::Assistant {
                text: resp.text.clone(),
                tool_calls: resp.tool_calls.clone(),
                reasoning: resp.reasoning.clone(),
            };
            save(&msg);
            messages.push(msg);
            if resp.tool_calls.is_empty() {
                return Ok(resp.text.unwrap_or_default());
            }
            let results = join_all(resp.tool_calls.iter().map(|tc| self.call(tc, sink))).await;
            for (tc, output) in resp.tool_calls.iter().zip(results) {
                let msg = Message::Tool { call_id: tc.id.clone(), name: tc.name.clone(), content: output };
                save(&msg);
                messages.push(msg);
            }
        }
    }

    async fn call(&self, tc: &ToolCall, sink: &dyn Sink) -> String {
        sink.emit(Event::ToolStart { name: tc.name.clone(), args: tc.arguments.to_string() });
        let output = if AGENT_SIDE.contains(&tc.name.as_str()) {
            self.call_here(tc).await.unwrap_or_else(|e| format!("error: {e:#}"))
        } else {
            self.exec.run_tool(&tc.name, &tc.arguments).await
        };
        sink.emit(Event::ToolEnd { name: tc.name.clone(), output: output.clone() });
        output
    }

    async fn call_here(&self, tc: &ToolCall) -> Result<String> {
        let a = &tc.arguments;
        match tc.name.as_str() {
            "update_plan" => Ok("Plan updated".into()),
            "web_search" => {
                let query = a["query"].as_str().unwrap_or_default();
                if self.provider.has_web_search() {
                    return self.provider.web_search(query).await;
                }
                let name = self.cfg.search_provider().ok_or_else(|| anyhow::anyhow!("no provider with web search"))?;
                llm::build(&self.cfg, &name, None)?.web_search(query).await
            }
            "spawn_agent" => {
                let task = a["task"].as_str().unwrap_or_default().to_string();
                let name = a["agent"].as_str().map(String::from).or(self.agent.clone());
                let mut child = Agent::new(&self.cfg, name.as_deref(), None, None, self.exec.clone())?;
                child.depth = self.depth + 1;
                let out = Box::pin(child.run(vec![Message::user(task)], &NullSink, &|_| {})).await?;
                Ok(if out.is_empty() { "(the agent finished without a final message)".into() } else { out })
            }
            _ => unreachable!(),
        }
    }
}
