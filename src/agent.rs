//! The agent loop: model turn → tool calls (in parallel) → results → repeat.

use crate::config::Config;
use crate::llm::{self, Provider, Request};
use crate::store::repair;
use crate::tools::{self, AGENT_SIDE, READ_ONLY};
use crate::types::{Ask, Event, Message, Sink, Tool, ToolCall, new_id, preview};
use anyhow::Result;
use futures::future::join_all;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What the agent may do without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Access {
    /// Everything (the default, and the only choice without someone to ask).
    #[default]
    Full,
    /// Commands and file changes wait for the person's approval.
    Ask,
    /// Only tools that look: no commands, no edits.
    ReadOnly,
}

impl Access {
    pub fn parse(s: &str) -> Access {
        match s {
            "ask" => Access::Ask,
            "read" | "read-only" | "readonly" => Access::ReadOnly,
            _ => Access::Full,
        }
    }
}

/// Tools a caller adds to the agent for one run (the interface tools of the
/// "ui" mode). They act on kron itself, never on the machine, so they need no
/// approval and stay available in read-only mode.
#[async_trait::async_trait]
pub trait Extra: Send + Sync {
    fn tools(&self) -> Vec<Tool>;
    /// Appended to the system prompt.
    fn system(&self) -> String;
    /// None: not one of these tools.
    async fn call(&self, name: &str, args: &Value, sink: &dyn Sink) -> Option<String>;
}

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
    /// Browse, read and upload files for the UI (see `files::handle`).
    async fn files(&self, req: &Value) -> Result<Value>;
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
    async fn files(&self, req: &Value) -> Result<Value> {
        let (cwd, req) = (self.ctx.cwd.clone(), req.clone());
        tokio::task::spawn_blocking(move || crate::files::handle(&cwd, &req)).await?
    }
}

pub fn environment(node: &str, cwd: &std::path::Path) -> String {
    let mut s = match crate::sandbox::get() {
        Some(sb) => format!(
            "<environment>\nmachine: {node}\nos: {}\ncwd: {}\nshell: bash\n</environment>",
            sb.describe(),
            sb.guest_path(cwd).display(),
        ),
        None => format!(
            "<environment>\nmachine: {node}\nos: {} ({})\ncwd: {}\nshell: {}\n</environment>",
            std::env::consts::OS,
            std::env::consts::ARCH,
            cwd.display(),
            if cfg!(windows) { "powershell" } else { "sh" },
        ),
    };
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

/// For small local models the Codex prompt is far too long.
const SMALL_PROMPT: &str = "You are kron, a helpful assistant running on the user's device. \
You can act on the device with tools: call `shell` to run commands, `write_file` to create files, \
`read_file` and `list_dir` to look around. When the user asks you to do something, do it with a tool \
call instead of explaining how. Relative paths are inside the working folder. \
Reply briefly, in the user's language.";

pub struct Agent {
    cfg: Arc<Config>,
    pub agent: Option<String>,
    provider: Arc<dyn Provider>,
    system: String,
    allowed: Vec<String>,
    exec: Arc<dyn Executor>,
    depth: usize,
    /// A small local model: short prompt, short tool set.
    small: bool,
    pub access: Access,
    /// "Allow everything" was answered once: shared with sub-agents for the rest of the run.
    allow_all: Arc<AtomicBool>,
    /// Parallel tool calls ask one at a time, so one answer can cover the rest.
    asking: Arc<tokio::sync::Mutex<()>>,
    pub extra: Option<Arc<dyn Extra>>,
}

/// A sub-agent's steps, shown under the spawn_agent call that started it.
struct ChildSink<'a> {
    parent: &'a dyn Sink,
    call: String,
}

impl Sink for ChildSink<'_> {
    fn emit(&self, e: Event) {
        match e {
            Event::ToolStart { id, name, args, parent } => {
                self.parent.emit(Event::ToolStart { id, name, args, parent: parent.or(Some(self.call.clone())) })
            }
            Event::ToolEnd { id, name, output, parent } => {
                self.parent.emit(Event::ToolEnd { id, name, output, parent: parent.or(Some(self.call.clone())) })
            }
            Event::Ask(_) | Event::Answered { .. } => self.parent.emit(e),
            _ => {}
        }
    }

    fn can_ask(&self) -> bool {
        self.parent.can_ask()
    }

    fn ask(&self, ask: Ask) -> Option<tokio::sync::oneshot::Receiver<String>> {
        self.parent.ask(ask)
    }
}

impl Agent {
    pub fn new(
        cfg: &Arc<Config>,
        agent: Option<&str>,
        provider: Option<&str>,
        model: Option<&str>,
        effort: Option<&str>,
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
        let provider = llm::build_with(cfg, &provider_name, model.or(a.model.as_deref()), effort)?;
        let small = cfg.provider(&provider_name)?.kind == "local";
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
            small,
            access: Access::Full,
            allow_all: Default::default(),
            asking: Default::default(),
            extra: None,
        })
    }

    fn tools(&self, interactive: bool) -> Vec<Tool> {
        let mut all = if self.small { tools::small() } else { tools::builtin() };
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
            all.retain(|t| self.allowed.contains(&t.name) || t.name == "ask_user");
        }
        if !interactive {
            all.retain(|t| t.name != "ask_user");
        }
        if self.access == Access::ReadOnly {
            all.retain(|t| READ_ONLY.contains(&t.name.as_str()));
        }
        if let Some(x) = &self.extra {
            all.extend(x.tools());
        }
        all
    }

    fn system_prompt(&self, with_tools: bool) -> String {
        if !with_tools {
            return self.system.clone();
        }
        let base = if self.small { SMALL_PROMPT } else { BASE_PROMPT };
        let mut s = format!("{base}\n\n{}", self.exec.environment());
        if self.depth == 0 && !self.small && !self.cfg.agents.is_empty() {
            s.push_str("\n\nAgents available to spawn_agent:");
            for (name, a) in &self.cfg.agents {
                let about =
                    a.system.as_deref().map(|t| preview(t.lines().next().unwrap_or(""), 120)).unwrap_or_default();
                s.push_str(&format!("\n- {name}: {about}"));
            }
        }
        match self.access {
            Access::ReadOnly => s.push_str(
                "\n\n<permissions>Read-only: you can look at files and the web, but cannot run commands or \
                change files. If the task needs changes, describe them instead.</permissions>",
            ),
            Access::Ask => s.push_str(
                "\n\n<permissions>The user approves each command and file change. If one is declined, do not \
                retry it; adjust the plan or ask.</permissions>",
            ),
            Access::Full => {}
        }
        if let Some(x) = &self.extra {
            s.push_str(&format!("\n\n{}", x.system()));
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
        let tools = self.tools(sink.can_ask());
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
        let args = tc.arguments.to_string();
        sink.emit(Event::ToolStart { id: tc.id.clone(), name: tc.name.clone(), args, parent: None });
        if let Some(x) = &self.extra
            && let Some(output) = x.call(&tc.name, &tc.arguments, sink).await
        {
            sink.emit(Event::ToolEnd {
                id: tc.id.clone(),
                name: tc.name.clone(),
                output: output.clone(),
                parent: None,
            });
            return output;
        }
        let output = match self.permit(tc, sink).await {
            Err(refusal) => refusal,
            Ok(()) if AGENT_SIDE.contains(&tc.name.as_str()) => {
                self.call_here(tc, sink).await.unwrap_or_else(|e| format!("error: {e:#}"))
            }
            Ok(()) => self.exec.run_tool(&tc.name, &tc.arguments).await,
        };
        sink.emit(Event::ToolEnd { id: tc.id.clone(), name: tc.name.clone(), output: output.clone(), parent: None });
        output
    }

    /// In "ask" mode, wait for the person to approve a call that changes things.
    async fn permit(&self, tc: &ToolCall, sink: &dyn Sink) -> Result<(), String> {
        if self.access == Access::ReadOnly && !READ_ONLY.contains(&tc.name.as_str()) {
            return Err("error: read-only mode, this tool is not available".into());
        }
        if self.access != Access::Ask || !tools::changes_things(&tc.name, &tc.arguments) {
            return Ok(());
        }
        let _one_question_at_a_time = self.asking.lock().await;
        if self.allow_all.load(Ordering::Relaxed) {
            return Ok(());
        }
        let ask = Ask {
            id: new_id(),
            kind: "approval".into(),
            name: tc.name.clone(),
            args: tc.arguments.to_string(),
            ..Default::default()
        };
        let Some(rx) = sink.ask(ask) else {
            return Err("error: this action needs the user's approval and nobody can give it now".into());
        };
        let answer = rx.await.unwrap_or_else(|_| "deny".into());
        match answer.as_str() {
            "allow" => Ok(()),
            "allow_all" => {
                self.allow_all.store(true, Ordering::Relaxed);
                Ok(())
            }
            other => {
                let why = other.strip_prefix("deny").unwrap_or(other).trim_start_matches(':').trim();
                Err(if why.is_empty() {
                    "The user declined this action. Do not retry it; choose another way or ask.".into()
                } else {
                    format!("The user declined this action: {why}")
                })
            }
        }
    }

    async fn call_here(&self, tc: &ToolCall, sink: &dyn Sink) -> Result<String> {
        let a = &tc.arguments;
        match tc.name.as_str() {
            "update_plan" => Ok("Plan updated".into()),
            "web_fetch" => {
                let n = |k: &str, d: u64| a[k].as_u64().or_else(|| a[k].as_f64().map(|f| f as u64)).unwrap_or(d);
                let url = a["url"].as_str().ok_or_else(|| anyhow::anyhow!("missing 'url'"))?;
                tools::web::fetch(url, n("offset", 0) as usize, n("max_chars", 20_000).clamp(1000, 100_000) as usize)
                    .await
            }
            "ask_user" => {
                let question = a["question"].as_str().unwrap_or_default().to_string();
                let options = a["options"]
                    .as_array()
                    .map(|o| o.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let ask = Ask { id: new_id(), kind: "question".into(), question, options, ..Default::default() };
                match sink.ask(ask) {
                    Some(rx) => Ok(match rx.await {
                        Ok(answer) => format!("The user answered: {answer}"),
                        Err(_) => "The question was dismissed. Continue with your best judgement.".into(),
                    }),
                    None => Ok("The user is not available. Continue with your best judgement.".into()),
                }
            }
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
                let mut child = Agent::new(&self.cfg, name.as_deref(), None, None, None, self.exec.clone())?;
                child.depth = self.depth + 1;
                child.access = self.access;
                child.allow_all = self.allow_all.clone();
                child.asking = self.asking.clone();
                let sink = ChildSink { parent: sink, call: tc.id.clone() };
                let out = Box::pin(child.run(vec![Message::user(task)], &sink, &|_| {})).await?;
                Ok(if out.is_empty() { "(the agent finished without a final message)".into() } else { out })
            }
            _ => unreachable!(),
        }
    }
}
