//! One user message → answer, in any mode. Shared by the CLI and the hub.

use crate::agent::{Agent, Executor};
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
    /// agent (default) | chat | web
    pub mode: String,
    pub agent: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Machine that runs the tools / the browser.
    pub node: Option<String>,
    /// Web chat site for mode = web (chatgpt, gemini, ...).
    pub site: Option<String>,
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
        "chat" => {
            let agent = Agent::new(cfg, req.agent.as_deref(), req.provider.as_deref(), req.model.as_deref(), exec)?;
            let answer = agent.chat(history, sink).await?;
            store.append(&req.thread, &Message::assistant(answer))?;
        }
        _ => {
            let agent = Agent::new(cfg, req.agent.as_deref(), req.provider.as_deref(), req.model.as_deref(), exec)?;
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
