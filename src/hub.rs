//! The hub: one process (on a server or your PC) that keeps threads and
//! credentials, runs the agent loops, serves the web UI, and sends tool calls
//! and web chats to whichever node (PC, phone, server) should execute them.

use crate::agent::{environment, Executor, Local};
use crate::config::Config;
use crate::store::{Record, Store};
use crate::turn::{turn, TurnReq};
use crate::types::{new_id, Event, Sink, Tool};
use anyhow::{bail, Result};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, oneshot, Notify};

const POLL_WAIT: Duration = Duration::from_secs(25);
const NODE_TTL: Duration = Duration::from_secs(60);

struct Node {
    info: Value,
    last_seen: Instant,
    queue: VecDeque<Value>,
    notify: Arc<Notify>,
}

struct Run {
    events: Mutex<Vec<Event>>,
    tx: broadcast::Sender<Event>,
}

impl Sink for Run {
    fn emit(&self, e: Event) {
        let mut events = self.events.lock().unwrap();
        events.push(e.clone());
        self.tx.send(e).ok();
    }
}

pub struct Hub {
    cfg: Arc<Config>,
    store: Store,
    name: String,
    workdir: std::path::PathBuf,
    nodes: Mutex<HashMap<String, Node>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    runs: Mutex<HashMap<String, Arc<Run>>>,
    thread_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

type Shared = Arc<Hub>;

pub async fn serve(cfg: Arc<Config>, workdir: std::path::PathBuf) -> Result<()> {
    let listen = cfg.hub.listen.clone().unwrap_or_else(|| "127.0.0.1:7700".into());
    let loopback = listen.starts_with("127.") || listen.starts_with("localhost") || listen.starts_with("[::1]");
    if cfg.hub.token.as_deref().unwrap_or("").is_empty() && !loopback {
        bail!("set [hub] token before listening on {listen}: nodes execute commands for whoever holds it");
    }
    let hub = Arc::new(Hub {
        store: Store::new(cfg.path("threads")),
        name: cfg.node_name(),
        workdir,
        cfg,
        nodes: Default::default(),
        pending: Default::default(),
        runs: Default::default(),
        thread_locks: Default::default(),
    });
    let app = Router::new()
        .route("/", get(|| async { Html(include_str!("ui.html")) }))
        .route("/api/info", get(info))
        .route("/api/threads", get(threads))
        .route("/api/threads/:name", get(thread))
        .route("/api/threads/:name/merge", post(merge))
        .route("/api/runs", post(start_run))
        .route("/api/runs/:id/events", get(run_events))
        .route("/api/nodes/:name/hello", post(node_hello))
        .route("/api/nodes/:name/poll", get(node_poll))
        .route("/api/nodes/:name/result", post(node_result))
        .with_state(hub.clone());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    eprintln!("kron hub on http://{listen} (this machine is node '{}')", hub.name);
    axum::serve(listener, app).await?;
    Ok(())
}

// ── Auth ──────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct TokenQuery {
    token: Option<String>,
}

fn authorized(hub: &Hub, headers: &HeaderMap, q: &TokenQuery) -> bool {
    let Some(expected) = hub.cfg.hub.token.as_deref().filter(|t| !t.is_empty()) else { return true };
    let given = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(String::from)
        .or_else(|| q.token.clone());
    given.as_deref() == Some(expected)
}

macro_rules! guard {
    ($hub:expr, $headers:expr, $q:expr) => {
        if !authorized(&$hub, &$headers, &$q) {
            return (StatusCode::UNAUTHORIZED, "bad or missing token").into_response();
        }
    };
}

fn err(e: impl std::fmt::Display) -> Response {
    (StatusCode::BAD_REQUEST, e.to_string()).into_response()
}

// ── Info & threads ────────────────────────────────────────────────────────────

impl Hub {
    fn online_nodes(&self) -> Vec<Value> {
        let mut nodes = self.nodes.lock().unwrap();
        nodes.retain(|_, n| n.last_seen.elapsed() < NODE_TTL * 10);
        let mut out = vec![
            json!({"name": self.name, "os": std::env::consts::OS, "cwd": self.workdir, "browser": !self.cfg.browser.disabled && crate::browser::find_chrome(&self.cfg.browser).is_some(), "hub": true}),
        ];
        for (name, n) in nodes.iter() {
            if n.last_seen.elapsed() < NODE_TTL && n.info["ephemeral"] != true {
                let mut v = n.info.clone();
                v["name"] = json!(name);
                v.as_object_mut().map(|o| o.remove("environment"));
                v.as_object_mut().map(|o| o.remove("tools"));
                out.push(v);
            }
        }
        out
    }
}

async fn info(State(hub): State<Shared>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    guard!(hub, headers, q);
    let providers: Vec<Value> =
        hub.cfg.providers.iter().map(|(n, p)| json!({"name": n, "kind": p.kind, "model": p.model})).collect();
    Json(json!({
        "hub": hub.name,
        "default": hub.cfg.default_provider().ok(),
        "nodes": hub.online_nodes(),
        "providers": providers,
        "agents": hub.cfg.agents.keys().collect::<Vec<_>>(),
        "sites": hub.cfg.sites.keys().collect::<Vec<_>>(),
    }))
    .into_response()
}

async fn threads(State(hub): State<Shared>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    guard!(hub, headers, q);
    Json(hub.store.list()).into_response()
}

async fn thread(
    State(hub): State<Shared>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    guard!(hub, headers, q);
    match hub.store.load(&name) {
        Ok(r) => Json(r).into_response(),
        Err(e) => err(e),
    }
}

async fn merge(
    State(hub): State<Shared>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Json(records): Json<Vec<Record>>,
) -> Response {
    guard!(hub, headers, q);
    match hub.store.merge(&name, records) {
        Ok(n) => Json(json!({"added": n})).into_response(),
        Err(e) => err(e),
    }
}

// ── Runs ──────────────────────────────────────────────────────────────────────

async fn start_run(
    State(hub): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Json(mut req): Json<TurnReq>,
) -> Response {
    guard!(hub, headers, q);
    if req.thread.is_empty() {
        req.thread = crate::store::new_thread_name();
    }
    if let Err(e) = crate::store::valid_name(&req.thread) {
        return err(e);
    }
    let exec: Arc<dyn Executor> = match req.node.as_deref() {
        None | Some("") => Arc::new(Local::new(&hub.cfg, hub.workdir.clone())),
        Some(n) if n == hub.name => Arc::new(Local::new(&hub.cfg, hub.workdir.clone())),
        Some(n) => match Remote::connect(&hub, n) {
            Ok(r) => Arc::new(r),
            Err(e) => return err(e),
        },
    };
    let id = new_id();
    let (tx, _) = broadcast::channel(4096);
    let run = Arc::new(Run { events: Mutex::new(Vec::new()), tx });
    hub.runs.lock().unwrap().insert(id.clone(), run.clone());

    let thread = req.thread.clone();
    let hub2 = hub.clone();
    let run_id = id.clone();
    tokio::spawn(async move {
        let lock = hub2.thread_locks.lock().unwrap().entry(req.thread.clone()).or_default().clone();
        let result = {
            let _one_turn_at_a_time = lock.lock().await;
            turn(&hub2.cfg, &hub2.store, &req, exec, run.as_ref()).await
        };
        match result {
            Ok(()) => run.emit(Event::Done),
            Err(e) => run.emit(Event::Error { message: format!("{e:#}") }),
        }
        // Keep finished runs around briefly for late subscribers.
        tokio::time::sleep(Duration::from_secs(300)).await;
        hub2.runs.lock().unwrap().remove(&run_id);
    });
    Json(json!({"run": id, "thread": thread})).into_response()
}

async fn run_events(
    State(hub): State<Shared>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    guard!(hub, headers, q);
    let Some(run) = hub.runs.lock().unwrap().get(&id).cloned() else {
        return (StatusCode::NOT_FOUND, "no such run").into_response();
    };
    let (snapshot, mut rx) = {
        let events = run.events.lock().unwrap();
        (events.clone(), run.tx.subscribe())
    };
    let (tx, out) = tokio::sync::mpsc::unbounded_channel::<Event>();
    tokio::spawn(async move {
        let mut finished = false;
        for e in snapshot {
            finished |= matches!(e, Event::Done | Event::Error { .. });
            if tx.send(e).is_err() {
                return;
            }
        }
        while !finished {
            match rx.recv().await {
                Ok(e) => {
                    finished = matches!(e, Event::Done | Event::Error { .. });
                    if tx.send(e).is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    });
    let stream = futures::stream::unfold(out, |mut out| async move {
        let e = out.recv().await?;
        Some((Ok::<_, std::convert::Infallible>(SseEvent::default().data(serde_json::to_string(&e).unwrap())), out))
    });
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

// ── Nodes ─────────────────────────────────────────────────────────────────────

async fn node_hello(
    State(hub): State<Shared>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Json(info): Json<Value>,
) -> Response {
    guard!(hub, headers, q);
    let mut nodes = hub.nodes.lock().unwrap();
    let node = nodes.entry(name.clone()).or_insert_with(|| Node {
        info: json!({}),
        last_seen: Instant::now(),
        queue: VecDeque::new(),
        notify: Arc::new(Notify::new()),
    });
    node.info = info;
    node.last_seen = Instant::now();
    eprintln!("node '{name}' connected");
    Json(json!({"ok": true})).into_response()
}

async fn node_poll(
    State(hub): State<Shared>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    guard!(hub, headers, q);
    let deadline = Instant::now() + POLL_WAIT;
    loop {
        let notify = {
            let mut nodes = hub.nodes.lock().unwrap();
            let Some(node) = nodes.get_mut(&name) else {
                return (StatusCode::CONFLICT, "say hello first").into_response();
            };
            node.last_seen = Instant::now();
            if let Some(job) = node.queue.pop_front() {
                return Json(job).into_response();
            }
            node.notify.clone()
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || tokio::time::timeout(left, notify.notified()).await.is_err() {
            return StatusCode::NO_CONTENT.into_response();
        }
    }
}

async fn node_result(
    State(hub): State<Shared>,
    Path(_name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Json(result): Json<Value>,
) -> Response {
    guard!(hub, headers, q);
    let id = result["id"].as_str().unwrap_or("").to_string();
    if let Some(tx) = hub.pending.lock().unwrap().remove(&id) {
        tx.send(result).ok();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Tools and web chats executed on a connected node.
struct Remote {
    hub: Arc<Hub>,
    node: String,
    info: Value,
}

impl Remote {
    fn connect(hub: &Arc<Hub>, node: &str) -> Result<Remote> {
        let nodes = hub.nodes.lock().unwrap();
        match nodes.get(node) {
            Some(n) if n.last_seen.elapsed() < NODE_TTL => {
                Ok(Remote { hub: hub.clone(), node: node.to_string(), info: n.info.clone() })
            }
            _ => bail!("node '{node}' is not connected"),
        }
    }

    async fn job(&self, mut job: Value, timeout: Duration) -> Result<Value> {
        let id = new_id();
        job["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.hub.pending.lock().unwrap().insert(id.clone(), tx);
        {
            let mut nodes = self.hub.nodes.lock().unwrap();
            let Some(n) = nodes.get_mut(&self.node) else { bail!("node '{}' disconnected", self.node) };
            n.queue.push_back(job);
            n.notify.notify_one();
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => match v["error"].as_str() {
                Some(e) => bail!("{e}"),
                None => Ok(v),
            },
            _ => {
                self.hub.pending.lock().unwrap().remove(&id);
                bail!("node '{}' did not answer", self.node)
            }
        }
    }
}

#[async_trait::async_trait]
impl Executor for Remote {
    async fn run_tool(&self, name: &str, args: &Value) -> String {
        match self.job(json!({"kind": "tool", "name": name, "args": args}), Duration::from_secs(1800)).await {
            Ok(v) => v["output"].as_str().unwrap_or("").to_string(),
            Err(e) => format!("error: {e:#}"),
        }
    }

    fn external_tools(&self) -> Vec<Tool> {
        serde_json::from_value(self.info["tools"].clone()).unwrap_or_default()
    }

    fn environment(&self) -> String {
        self.info["environment"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| environment(&self.node, std::path::Path::new(self.info["cwd"].as_str().unwrap_or("."))))
    }

    async fn web_chat(&self, site: &str, prompt: &str, url: Option<&str>) -> Result<(String, String)> {
        let v = self
            .job(json!({"kind": "web", "site": site, "prompt": prompt, "url": url}), Duration::from_secs(900))
            .await?;
        Ok((v["output"].as_str().unwrap_or("").into(), v["url"].as_str().unwrap_or("").into()))
    }
}
