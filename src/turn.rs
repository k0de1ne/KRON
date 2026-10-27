//! One user message → answer, in any mode. Shared by the CLI and the hub.

use crate::agent::{Access, Agent, Executor};
use crate::config::Config;
use crate::store::Store;
use crate::types::{Event, Message, Sink};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TurnReq {
    pub thread: String,
    pub text: String,
    /// agent (default) | chat | web | ui (the agent builds an interface)
    pub mode: String,
    pub agent: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Machine that runs the tools / the browser.
    pub node: Option<String>,
    /// Web chat site for mode = web (chatgpt, gemini, ...).
    pub site: Option<String>,
    /// Reasoning effort override (minimal | low | medium | high).
    pub effort: Option<String>,
    /// full (default) | ask | read
    pub access: String,
    /// Working folder on the node (default: the node's own).
    pub cwd: Option<String>,
    /// Someone is watching (the web UI) and can answer approvals and questions.
    pub interactive: bool,
}

pub async fn turn(
    cfg: &Arc<Config>,
    store: &Store,
    req: &TurnReq,
    exec: Arc<dyn Executor>,
    sink: &dyn Sink,
) -> Result<()> {
    let user = Message::user(req.text.clone());
    let mut history = store.messages(&req.thread)?;
    history.push(user.clone());
    store.append(&req.thread, &user)?;

    match req.mode.as_str() {
        "web" => {
            let site = req.site.clone().unwrap_or_else(|| "chatgpt".into());
            let meta = store.meta(&req.thread);
            let url = meta["web"][&site].as_str().map(String::from);
            let (answer, url) = exec.web_chat(&site, &req.text, url.as_deref()).await?;
            sink.emit(Event::Text { content: answer.clone() });
            store.append(&req.thread, &Message::assistant(answer))?;
            let mut web = meta["web"].clone();
            if !web.is_object() {
                web = json!({});
            }
            web[&site] = json!(url);
            store.set_meta(&req.thread, "web", web)?;
        }
        "ui" if crate::genui::capable(cfg, req) => {
            crate::genui::turn(cfg, store, req, exec, sink, history).await?;
        }
        "chat" | "ui" => {
            let agent = new_agent(cfg, req, exec)?;
            let answer = agent.chat(history, sink).await?;
            store.append(&req.thread, &Message::assistant(answer))?;
        }
        _ => {
            let mut agent = new_agent(cfg, req, exec)?;
            agent.access = Access::parse(&req.access);
            let thread = req.thread.clone();
            agent
                .run(history, sink, &|m| {
                    if let Err(e) = store.append(&thread, m) {
                        eprintln!("cannot save message: {e}");
                    }
                })
                .await?;
        }
    }
    Ok(())
}

fn new_agent(cfg: &Arc<Config>, req: &TurnReq, exec: Arc<dyn Executor>) -> Result<Agent> {
    let (agent, provider) = (req.agent.as_deref(), req.provider.as_deref());
    Agent::new(cfg, agent, provider, req.model.as_deref(), req.effort.as_deref(), exec)
}
