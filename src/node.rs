//! Talking to the hub: the HTTP client used by the CLI, and the node worker
//! that executes tool calls and web chats for the hub on this machine.

use crate::agent::{Executor, Local, environment};
use crate::config::Config;
use crate::llm::{check, http, sse};
use crate::store::{Record, ThreadInfo};
use crate::turn::TurnReq;
use crate::types::{Event, Sink};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
}

impl Client {
    pub fn from_config(cfg: &Config) -> Option<Client> {
        let url = cfg.hub.url.clone()?;
        Some(Client { base: url.trim_end_matches('/').to_string(), token: cfg.hub.token.clone() })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let rb = http().request(method, format!("{}{path}", self.base));
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    pub async fn info(&self) -> Result<Value> {
        Ok(check(self.req(reqwest::Method::GET, "/api/info").send().await?).await?.json().await?)
    }

    pub async fn threads(&self) -> Result<Vec<ThreadInfo>> {
        Ok(check(self.req(reqwest::Method::GET, "/api/threads").send().await?).await?.json().await?)
    }

    pub async fn thread(&self, name: &str) -> Result<Vec<Record>> {
        Ok(check(self.req(reqwest::Method::GET, &format!("/api/threads/{name}")).send().await?).await?.json().await?)
    }

    pub async fn merge(&self, name: &str, records: &[Record]) -> Result<usize> {
        let v: Value =
            check(self.req(reqwest::Method::POST, &format!("/api/threads/{name}/merge")).json(records).send().await?)
                .await?
                .json()
                .await?;
        Ok(v["added"].as_u64().unwrap_or(0) as usize)
    }

    /// Start a turn on the hub and stream its events into `sink`.
    pub async fn run(&self, req: &TurnReq, sink: &dyn Sink) -> Result<String> {
        let v: Value =
            check(self.req(reqwest::Method::POST, "/api/runs").json(req).send().await?).await?.json().await?;
        let id = v["run"].as_str().context("hub returned no run id")?;
        let resp = check(self.req(reqwest::Method::GET, &format!("/api/runs/{id}/events")).send().await?).await?;
        let mut failure = None;
        sse(resp, |_, data| {
            if let Ok(e) = serde_json::from_str::<Event>(data) {
                if let Event::Error { message } = &e {
                    failure = Some(message.clone());
                    return Ok(());
                }
                sink.emit(e);
            }
            Ok(())
        })
        .await?;
        if let Some(m) = failure {
            bail!("{m}");
        }
        Ok(v["thread"].as_str().unwrap_or("").to_string())
    }
}

/// Serve tool calls and web chats for the hub until cancelled.
/// `ready` fires after the first successful hello; `ephemeral` hides the node
/// from listings (a CLI session serving its own tool calls).
pub async fn work(
    cfg: Arc<Config>,
    client: Client,
    name: String,
    cwd: std::path::PathBuf,
    ephemeral: bool,
    mut ready: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<()> {
    let local = Arc::new(Local::new(&cfg, cwd.clone()));
    let mut announced = false;
    loop {
        let hello = json!({
            "ephemeral": ephemeral,
            "os": std::env::consts::OS,
            "cwd": cwd,
            "browser": !cfg.browser.disabled && crate::browser::find_chrome(&cfg.browser).is_some(),
            "environment": environment(&name, &cwd),
            "tools": local.external_tools(),
        });
        let path = format!("/api/nodes/{name}");
        match client.req(reqwest::Method::POST, &format!("{path}/hello")).json(&hello).send().await {
            Ok(r) if r.status().is_success() => {
                if !announced && !ephemeral {
                    eprintln!("connected to hub {} as node '{name}'", client.base);
                }
                announced = true;
                if let Some(tx) = ready.take() {
                    tx.send(()).ok();
                }
            }
            Ok(r) if r.status() == 401 => bail!("hub rejected the token"),
            other => {
                eprintln!(
                    "hub unreachable ({}), retrying…",
                    other.map(|r| r.status().to_string()).unwrap_or_else(|e| e.to_string())
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        }
        loop {
            let resp =
                client.req(reqwest::Method::GET, &format!("{path}/poll")).timeout(Duration::from_secs(60)).send().await;
            let job: Value = match resp {
                Ok(r) if r.status() == 204 => continue,
                Ok(r) if r.status().is_success() => match r.json().await {
                    Ok(j) => j,
                    Err(_) => continue,
                },
                // Hub restarted and forgot us, or the network dropped: say hello again.
                _ => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    break;
                }
            };
            let (client, local, path) = (client.clone(), local.clone(), path.clone());
            tokio::spawn(async move {
                let id = job["id"].clone();
                // A thread can have its own working folder on this machine.
                let local = match job["cwd"].as_str().filter(|c| !c.is_empty()) {
                    Some(c) => Arc::new(Local::new(&local.cfg, crate::files::resolve(&local.ctx.cwd, c))),
                    None => local,
                };
                let result = match job["kind"].as_str() {
                    Some("tool") => {
                        let name = job["name"].as_str().unwrap_or("");
                        json!({"id": id, "output": local.run_tool(name, &job["args"]).await})
                    }
                    Some("files") => match local.files(&job["req"]).await {
                        Ok(v) => json!({"id": id, "output": v}),
                        Err(e) => json!({"id": id, "error": format!("{e:#}")}),
                    },
                    Some("env") => json!({"id": id, "output": local.environment()}),
                    Some("web") => {
                        let site = job["site"].as_str().unwrap_or("chatgpt");
                        match local.web_chat(site, job["prompt"].as_str().unwrap_or(""), job["url"].as_str()).await {
                            Ok((output, url)) => json!({"id": id, "output": output, "url": url}),
                            Err(e) => json!({"id": id, "error": format!("{e:#}")}),
                        }
                    }
                    _ => json!({"id": id, "error": "unknown job"}),
                };
                for _ in 0..3 {
                    match client.req(reqwest::Method::POST, &format!("{path}/result")).json(&result).send().await {
                        Ok(r) if r.status().is_success() => break,
                        _ => tokio::time::sleep(Duration::from_secs(2)).await,
                    }
                }
            });
        }
    }
}
