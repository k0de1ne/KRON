//! The hub: one process (on a server or your PC) that keeps threads and
//! credentials, runs the agent loops, serves the web UI, and sends tool calls
//! and web chats to whichever node (PC, phone, server) should execute them.

use crate::agent::{Executor, Local, environment};
use crate::config::Config;
use crate::store::{Record, Store};
use crate::turn::{TurnReq, turn};
use crate::types::{Event, Sink, Tool, new_id};
use anyhow::{Result, bail};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, broadcast, oneshot};

const POLL_WAIT: Duration = Duration::from_secs(25);
const NODE_TTL: Duration = Duration::from_secs(60);

struct Node {
    info: Value,
    last_seen: Instant,
    queue: VecDeque<Value>,
    notify: Arc<Notify>,
}

struct Run {
    thread: String,
    /// Records the thread had (including this turn's user message) when the run started:
    /// a client that attaches later shows those, then replays the events.
    base: usize,
    events: Mutex<Vec<Event>>,
    tx: broadcast::Sender<Event>,
    abort: Mutex<Option<tokio::task::AbortHandle>>,
    finished: std::sync::atomic::AtomicBool,
}

impl Sink for Run {
    fn emit(&self, e: Event) {
        let mut events = self.events.lock().unwrap();
        events.push(e.clone());
        self.tx.send(e).ok();
    }
}

pub struct Hub {
    /// Replaced when the config is saved from the web UI.
    cfg: std::sync::RwLock<Arc<Config>>,
    store: Store,
    name: String,
    workdir: std::path::PathBuf,
    nodes: Mutex<HashMap<String, Node>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    runs: Mutex<HashMap<String, Arc<Run>>>,
    thread_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    logins: Mutex<HashMap<String, Login>>,
}

struct Login {
    flow: Arc<crate::auth::Flow>,
    status: Arc<Mutex<Value>>,
    task: tokio::task::AbortHandle,
}

impl Hub {
    fn cfg(&self) -> Arc<Config> {
        self.cfg.read().unwrap().clone()
    }

    /// Re-read config.toml (and the models directory).
    fn reload(&self) -> Result<()> {
        let file = self.cfg().file.clone();
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        *self.cfg.write().unwrap() = Arc::new(Config::parse(&text, file)?);
        Ok(())
    }
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
        cfg: std::sync::RwLock::new(cfg),
        nodes: Default::default(),
        pending: Default::default(),
        runs: Default::default(),
        thread_locks: Default::default(),
        logins: Default::default(),
    });
    let app = Router::new()
        .route("/", get(|| async { Html(include_str!("ui.html")) }))
        .route("/api/info", get(info))
        .route("/api/threads", get(threads))
        .route("/api/threads/:name", get(thread).delete(delete_thread))
        .route("/api/threads/:name/run", get(thread_run))
        .route("/api/threads/:name/merge", post(merge))
        .route("/api/runs", post(start_run))
        .route("/api/runs/:id/events", get(run_events))
        .route("/api/runs/:id/cancel", post(cancel_run))
        .route("/manifest.webmanifest", get(manifest))
        .route("/sw.js", get(|| async { ([("content-type", "text/javascript")], SW_JS) }))
        .route("/icon.svg", get(|| async { ([("content-type", "image/svg+xml")], ICON_SVG) }))
        .route(
            "/icon-192.png",
            get(|| async { ([("content-type", "image/png")], &include_bytes!("assets/icon-192.png")[..]) }),
        )
        .route(
            "/icon-512.png",
            get(|| async { ([("content-type", "image/png")], &include_bytes!("assets/icon-512.png")[..]) }),
        )
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/models", get(models))
        .route("/api/models/:file", post(model_download).delete(model_delete))
        .route("/api/login/:provider", get(login_status).post(login_start))
        .route("/api/login/:provider/code", post(login_code))
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
    let cfg = hub.cfg();
    let Some(expected) = cfg.hub.token.as_deref().filter(|t| !t.is_empty()) else { return true };
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
            json!({"name": self.name, "os": std::env::consts::OS, "cwd": self.workdir, "browser": !self.cfg().browser.disabled && crate::browser::find_chrome(&self.cfg().browser).is_some(), "hub": true}),
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
    let cfg = hub.cfg();
    let providers: Vec<Value> = cfg
        .providers
        .iter()
        .map(|(n, p)| {
            let oauth = matches!(p.kind.as_str(), "codex" | "claude" | "gemini" | "antigravity");
            let logged_in = oauth && crate::auth::Auth::new(&cfg, n, &p.kind, p).is_ok_and(|a| a.logged_in());
            json!({"name": n, "kind": p.kind, "model": p.model, "oauth": oauth, "logged_in": logged_in})
        })
        .collect();
    Json(json!({
        "hub": hub.name,
        "default": cfg.default_provider().ok(),
        "nodes": hub.online_nodes(),
        "providers": providers,
        "agents": cfg.agents.keys().collect::<Vec<_>>(),
        "sites": cfg.sites.keys().collect::<Vec<_>>(),
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

// ── Settings & sign-in ───────────────────────────────────────────────────────

async fn get_config(State(hub): State<Shared>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    guard!(hub, headers, q);
    let cfg = hub.cfg();
    let text = std::fs::read_to_string(&cfg.file).unwrap_or_else(|_| crate::config::EXAMPLE.to_string());
    Json(json!({"path": cfg.file, "text": text})).into_response()
}

/// Save config.toml and apply it without a restart (listen address and
/// token take effect on the next start).
async fn put_config(
    State(hub): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Json(body): Json<Value>,
) -> Response {
    guard!(hub, headers, q);
    let text = body["text"].as_str().unwrap_or("");
    let file = hub.cfg().file.clone();
    let new = match Config::parse(text, file.clone()) {
        Ok(c) => c,
        Err(e) => return err(format!("{e:#}")),
    };
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if let Err(e) = std::fs::write(&file, text) {
        return err(e);
    }
    *hub.cfg.write().unwrap() = Arc::new(new);
    Json(json!({"ok": true})).into_response()
}

/// Start an OAuth sign-in. The browser's redirect to localhost is caught when
/// it runs on this machine; otherwise the UI posts the pasted URL / code.
async fn login_start(
    State(hub): State<Shared>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    guard!(hub, headers, q);
    let cfg = hub.cfg();
    let flow = match crate::auth::begin(&cfg, &provider) {
        Ok(f) => Arc::new(f),
        Err(e) => return err(format!("{e:#}")),
    };
    if let Some(old) = hub.logins.lock().unwrap().remove(&provider) {
        old.task.abort();
    }
    let status = Arc::new(Mutex::new(json!({"state": "waiting"})));
    let (flow2, status2) = (flow.clone(), status.clone());
    // Bind the callback port before replying, so a fast redirect is not missed.
    let task = tokio::spawn(async move {
        let url = flow2.callback().await;
        let result = flow2.finish(&cfg, &url).await;
        *status2.lock().unwrap() = match result {
            Ok(_) => json!({"state": "done"}),
            Err(e) => json!({"state": "error", "error": format!("{e:#}")}),
        };
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let url = flow.url.clone();
    let paste = flow.port.is_none();
    hub.logins.lock().unwrap().insert(provider, Login { flow, status, task: task.abort_handle() });
    Json(json!({"url": url, "paste": paste})).into_response()
}

async fn login_status(
    State(hub): State<Shared>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    guard!(hub, headers, q);
    let v = hub.logins.lock().unwrap().get(&provider).map(|l| l.status.lock().unwrap().clone());
    Json(v.unwrap_or(json!({"state": "none"}))).into_response()
}

async fn login_code(
    State(hub): State<Shared>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Json(body): Json<Value>,
) -> Response {
    guard!(hub, headers, q);
    let Some((flow, status)) = hub.logins.lock().unwrap().get(&provider).map(|l| (l.flow.clone(), l.status.clone()))
    else {
        return err("no sign-in in progress");
    };
    match flow.finish(&hub.cfg(), body["code"].as_str().unwrap_or("")).await {
        Ok(_) => {
            if let Some(l) = hub.logins.lock().unwrap().remove(&provider) {
                l.task.abort();
            }
            *status.lock().unwrap() = json!({"state": "done"});
            Json(json!({"state": "done"})).into_response()
        }
        Err(e) => err(format!("{e:#}")),
    }
}

// ── Local models ─────────────────────────────────────────────────────────────

async fn models(State(hub): State<Shared>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    guard!(hub, headers, q);
    let cfg = hub.cfg();
    let dir = crate::local::models_dir(&cfg.home);
    let list: Vec<Value> = crate::local::CATALOG
        .iter()
        .map(|m| {
            json!({
                "file": m.file, "title": m.title, "note": m.note, "size": m.size,
                "installed": dir.join(m.file).is_file(),
                "progress": crate::local::progress(m.file),
            })
        })
        .collect();
    Json(json!({"models": list, "server": crate::local::find_server().is_some()})).into_response()
}

async fn model_download(
    State(hub): State<Shared>,
    Path(file): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    guard!(hub, headers, q);
    let hub2 = hub.clone();
    match crate::local::download(&hub.cfg().home, &file, move || {
        hub2.reload().ok();
    }) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => err(e),
    }
}

async fn model_delete(
    State(hub): State<Shared>,
    Path(file): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    guard!(hub, headers, q);
    if let Err(e) = crate::local::remove(&hub.cfg().home, &file).await {
        return err(format!("{e:#}"));
    }
    hub.reload().ok();
    Json(json!({"ok": true})).into_response()
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
        None | Some("") => Arc::new(Local::new(&hub.cfg(), hub.workdir.clone())),
        Some(n) if n == hub.name => Arc::new(Local::new(&hub.cfg(), hub.workdir.clone())),
        Some(n) => match Remote::connect(&hub, n) {
            Ok(r) => Arc::new(r),
            Err(e) => return err(e),
        },
    };
    let id = new_id();
    let (tx, _) = broadcast::channel(4096);
    let run = Arc::new(Run {
        thread: req.thread.clone(),
        base: hub.store.load(&req.thread).map(|r| r.len()).unwrap_or(0) + 1,
        events: Mutex::new(Vec::new()),
        tx,
        abort: Mutex::new(None),
        finished: Default::default(),
    });
    hub.runs.lock().unwrap().insert(id.clone(), run.clone());

    let thread = req.thread.clone();
    let hub2 = hub.clone();
    let run_id = id.clone();
    let run2 = run.clone();
    let work = tokio::spawn(async move {
        let lock = hub2.thread_locks.lock().unwrap().entry(req.thread.clone()).or_default().clone();
        let _one_turn_at_a_time = lock.lock().await;
        turn(&hub2.cfg(), &hub2.store, &req, exec, run2.as_ref()).await
    });
    *run.abort.lock().unwrap() = Some(work.abort_handle());
    let hub2 = hub.clone();
    tokio::spawn(async move {
        let result = work.await;
        run.finished.store(true, std::sync::atomic::Ordering::Relaxed);
        match result {
            Ok(Ok(())) => run.emit(Event::Done),
            Ok(Err(e)) => run.emit(Event::Error { message: format!("{e:#}") }),
            Err(_) => run.emit(Event::Error { message: "stopped".into() }),
        }
        // Keep finished runs around briefly for late subscribers.
        tokio::time::sleep(Duration::from_secs(300)).await;
        hub2.runs.lock().unwrap().remove(&run_id);
    });
    Json(json!({"run": id, "thread": thread})).into_response()
}

async fn cancel_run(
    State(hub): State<Shared>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    guard!(hub, headers, q);
    let run = hub.runs.lock().unwrap().get(&id).cloned();
    if let Some(h) = run.and_then(|r| r.abort.lock().unwrap().take()) {
        h.abort();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// The run currently producing an answer in this thread, if any.
async fn thread_run(
    State(hub): State<Shared>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    guard!(hub, headers, q);
    let runs = hub.runs.lock().unwrap();
    let active = runs
        .iter()
        .find(|(_, r)| r.thread == name && !r.finished.load(std::sync::atomic::Ordering::Relaxed))
        .map(|(id, r)| json!({"run": id, "base": r.base}));
    Json(active.unwrap_or(json!({"run": null}))).into_response()
}

async fn delete_thread(
    State(hub): State<Shared>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Response {
    guard!(hub, headers, q);
    match hub.store.delete(&name) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(e),
    }
}

async fn manifest() -> Response {
    let m = json!({
        "name": "kron", "short_name": "kron", "start_url": "/", "scope": "/", "display": "standalone",
        "background_color": "#1f1e1d", "theme_color": "#1f1e1d",
        "icons": [
            {"src": "/icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any maskable"},
            {"src": "/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any maskable"},
            {"src": "/icon.svg", "sizes": "any", "type": "image/svg+xml"}
        ]
    });
    ([("content-type", "application/manifest+json")], m.to_string()).into_response()
}

/// Lets the web UI install as an app (Android / desktop Chrome). Never caches the API.
const SW_JS: &str = "self.addEventListener('install', () => self.skipWaiting());\n\
self.addEventListener('activate', e => e.waitUntil(self.clients.claim()));\n\
self.addEventListener('fetch', () => {});\n";

pub const ICON_SVG: &str = include_str!("assets/icon.svg");

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
