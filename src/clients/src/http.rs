use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Json, Response,
    },
    routing::{get, post},
    Router,
};
use futures::StreamExt;
use proto::{AgentEvent, Config, EventSink, Message};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub backend: Arc<dyn proto::LlmBackend>,
}

pub async fn serve(state: AppState, port: u16) -> Result<()> {
    let app = Router::new()
        .route("/", get(ui))
        .route("/manifest.json", get(manifest))
        .route("/agents", get(list_agents))
        .route("/agents/:agent/threads", get(list_threads))
        .route("/agents/:agent/history/:thread", get(get_history))
        .route("/run", post(run_task))
        .with_state(state);

    let ip = local_ip();
    eprintln!("[plexus] http://{}:{}", ip, port);
    eprintln!("[plexus] http://localhost:{}", port);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| { s.connect("8.8.8.8:80")?; s.local_addr() })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "localhost".to_string())
}

async fn ui() -> Html<&'static str> { Html(UI_HTML) }

async fn manifest() -> Json<Value> {
    Json(json!({
        "name": "PLEXUS",
        "short_name": "PLEXUS",
        "start_url": "/",
        "display": "standalone",
        "background_color": "#0d0d0d",
        "theme_color": "#0d0d0d",
        "icons": []
    }))
}

async fn list_agents() -> Json<Vec<String>> {
    let mut agents: Vec<String> = std::fs::read_dir("..")
        .into_iter().flatten().flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    agents.sort();
    Json(agents)
}

// Resolve an agent's threads directory: always ../agentname/threads
fn agent_threads_dir(agent: &str) -> std::path::PathBuf {
    std::path::Path::new("..").join(agent).join("threads")
}

// Load an agent's config; fall back to default on error
fn agent_config(agent: &str) -> Config {
    let path = std::path::Path::new("..").join(agent).join("config.toml");
    Config::load(Some(&path)).unwrap_or_default()
}

async fn list_threads(Path(agent): Path<String>) -> Json<Vec<String>> {
    let mut threads: Vec<String> = std::fs::read_dir(agent_threads_dir(&agent))
        .into_iter().flatten().flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension()?.to_str()? == "jsonl")
                .then(|| p.file_stem()?.to_str().map(String::from))?
        })
        .collect();
    threads.sort();
    Json(threads)
}

async fn get_history(Path((agent, thread)): Path<(String, String)>) -> Json<Vec<Value>> {
    let Ok(sess) = session::Session::open(&thread, None, &agent_threads_dir(&agent)) else {
        return Json(vec![]);
    };
    let msgs = sess.messages().iter().filter_map(|m| match m {
        Message::User(c) => Some(json!({"role": "user", "content": c})),
        Message::Assistant { text: Some(t), .. } => Some(json!({"role": "assistant", "content": t})),
        _ => None,
    }).collect();
    Json(msgs)
}

#[derive(Deserialize)]
struct RunRequest {
    agent: String,
    task: String,
    thread: String,
}

async fn run_task(
    State(state): State<AppState>,
    Json(req): Json<RunRequest>,
) -> Response {
    if req.task.is_empty() || req.thread.is_empty() || req.agent.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let cfg = agent_config(&req.agent);
    let api_key = cfg.api_key().or_else(|| state.cfg.api_key());
    let backend = match llm::create(&cfg.llm, api_key) {
        Ok(b) => b,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let tx2 = tx.clone();
    let sink = Arc::new(ChannelSink { tx });
    let threads_dir = agent_threads_dir(&req.agent);
    let thread = req.thread.clone();
    let task = req.task.clone();

    tokio::spawn(async move {
        let result = match session::Session::open(&thread, Some(cfg), &threads_dir) {
            Ok(mut sess) => sess.run(task, backend, sink).await,
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            tx2.send(AgentEvent::Text { content: format!("\u{26a0} error: {e}") }).ok();
            tx2.send(AgentEvent::Done).ok();
        }
    });

    let stream = UnboundedReceiverStream::new(rx).map(|ev| {
        Ok::<_, std::convert::Infallible>(
            Event::default().data(serde_json::to_string(&ev).unwrap_or_default())
        )
    });

    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

struct ChannelSink { tx: mpsc::UnboundedSender<AgentEvent> }

#[async_trait::async_trait]
impl EventSink for ChannelSink {
    async fn emit(&self, event: AgentEvent) { self.tx.send(event).ok(); }
}

// ── Embedded UI ───────────────────────────────────────────────────────────────

static UI_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1,maximum-scale=1,viewport-fit=cover">
<meta name="apple-mobile-web-app-capable" content="yes">
<meta name="apple-mobile-web-app-status-bar-style" content="black-translucent">
<link rel="manifest" href="/manifest.json">
<title>PLEXUS</title>
<style>
*{box-sizing:border-box;margin:0;padding:0;-webkit-tap-highlight-color:transparent}
:root{--bg:#0d0d0d;--bg2:#111;--bg3:#171717;--fg:#d0d0d0;--fg2:#666;--blue:#5da7f5;--border:#1e1e1e}
html,body{height:100%;background:var(--bg);color:var(--fg);font-family:'JetBrains Mono','Fira Mono',Consolas,monospace;font-size:14px;line-height:1.6;overflow:hidden}
body{display:flex;flex-direction:column;height:100dvh}

#hdr{display:flex;align-items:center;gap:8px;padding:10px 14px;border-bottom:1px solid var(--border);flex-shrink:0}
#hdr-title{flex:1;font-size:11px;color:var(--blue);letter-spacing:.12em;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.hdr-btn{background:none;border:1px solid var(--border);border-radius:5px;color:var(--fg2);font-size:15px;width:32px;height:32px;cursor:pointer;display:flex;align-items:center;justify-content:center;flex-shrink:0}
.hdr-btn:active{background:var(--bg3)}

#msgs{flex:1;overflow-y:auto;padding:16px 14px;display:flex;flex-direction:column;gap:12px;overscroll-behavior:contain}
.msg{max-width:100%;word-break:break-word;white-space:pre-wrap}
.user{align-self:flex-end;background:var(--bg3);border:1px solid var(--border);border-radius:12px 12px 3px 12px;padding:8px 12px;color:var(--blue);max-width:85%}
.assistant{align-self:flex-start;color:var(--fg)}
.assistant.streaming::after{content:'▋';animation:blink .7s step-end infinite;margin-left:2px}
.tool{align-self:flex-start;display:inline-flex;align-items:center;gap:6px;font-size:12px;border-radius:6px;padding:4px 10px;background:var(--bg3);border:1px solid var(--border);max-width:90%;overflow:hidden}
.tool .ti{font-size:10px;flex-shrink:0}
.tool .tn{color:var(--blue);font-weight:600;flex-shrink:0}
.tool .ta{color:var(--fg2);overflow:hidden;text-overflow:ellipsis;white-space:nowrap;min-width:0}
.tool.running{border-color:#444}
.tool.running .ti{animation:spin 1.2s linear infinite;display:inline-block}
.tool.done .ti{color:#4a9}
.tool.done .tn{color:var(--fg2)}
details.think{align-self:flex-start;max-width:100%;border-left:2px solid #2a2a2a;padding-left:8px}
details.think summary{font-size:11px;color:var(--fg2);cursor:pointer;letter-spacing:.05em;list-style:none}
details.think summary::before{content:'▶ '}
details.think[open] summary::before{content:'▼ '}
details.think .tc{font-size:12px;color:#444;white-space:pre-wrap;word-break:break-word;margin-top:4px}
.assistant p{margin:.2em 0}.assistant p:first-child{margin-top:0}.assistant p:last-child{margin-bottom:0}
.assistant strong{color:#e0e0e0}
.assistant em{color:var(--fg2);font-style:italic}
.assistant code{background:var(--bg3);border-radius:3px;padding:1px 5px;font-size:12px;font-family:inherit}
.assistant pre{background:var(--bg3);border:1px solid var(--border);border-radius:6px;padding:10px;overflow-x:auto;margin:6px 0}
.assistant pre code{background:none;padding:0;font-size:12px}
.assistant ul,.assistant ol{padding-left:18px;margin:4px 0}
.assistant li{margin:2px 0}
.assistant h3,.assistant h4,.assistant h5{margin:6px 0 2px;color:#e0e0e0}
@keyframes blink{50%{opacity:0}}
@keyframes spin{to{transform:rotate(360deg)}}

#inp-wrap{border-top:1px solid var(--border);padding:10px 14px;display:flex;gap:8px;align-items:flex-end;flex-shrink:0;padding-bottom:max(10px,env(safe-area-inset-bottom))}
#inp{flex:1;background:var(--bg2);border:1px solid var(--border);border-radius:8px;color:var(--fg);font-family:inherit;font-size:14px;padding:9px 12px;outline:none;resize:none;min-height:40px;max-height:120px;line-height:1.5}
#inp:focus{border-color:#2a2a2a}
#send{background:var(--blue);border:none;border-radius:8px;color:#0d0d0d;font-size:18px;font-weight:700;width:40px;height:40px;cursor:pointer;flex-shrink:0;display:flex;align-items:center;justify-content:center}
#send:disabled{background:var(--border);color:var(--fg2);cursor:default}

#panel{position:fixed;inset:0;z-index:10;display:flex;pointer-events:none}
#panel-bg{flex:1;background:rgba(0,0,0,.55);opacity:0;transition:opacity .2s;pointer-events:none}
#panel-body{width:min(260px,78vw);background:var(--bg2);border-right:1px solid var(--border);transform:translateX(-100%);transition:transform .2s;display:flex;flex-direction:column;overflow:hidden;pointer-events:all}
#panel.open{pointer-events:all}
#panel.open #panel-bg{opacity:1;pointer-events:all}
#panel.open #panel-body{transform:translateX(0)}
#panel-hdr{display:flex;align-items:center;justify-content:flex-end;padding:6px 8px;border-bottom:1px solid var(--border)}
#panel-scroll{overflow-y:auto;flex:1;padding:6px}
.psec{font-size:10px;color:var(--fg2);letter-spacing:.1em;padding:10px 12px 4px}
.tb{display:block;width:100%;text-align:left;background:none;border:none;color:var(--fg);font-family:inherit;font-size:13px;padding:9px 12px;border-radius:6px;cursor:pointer;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.tb:active,.tb:hover{background:var(--bg3)}
.tb.on{color:var(--blue)}
.tb.agent-btn{color:var(--fg2)}
.tb.agent-btn.on{color:var(--blue)}

#notif-bar{display:none;position:fixed;bottom:80px;left:50%;transform:translateX(-50%);background:var(--blue);color:#0d0d0d;padding:8px 18px;border-radius:20px;font-size:13px;pointer-events:none;z-index:20}
</style>
</head>
<body>

<div id="hdr">
  <button class="hdr-btn" id="btn-menu">☰</button>
  <div id="hdr-title">PLEXUS</div>
  <button class="hdr-btn" id="btn-new">＋</button>
</div>

<div id="msgs"></div>

<div id="inp-wrap">
  <textarea id="inp" rows="1" placeholder="message…" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"></textarea>
  <button id="send">↑</button>
</div>

<div id="panel">
  <div id="panel-body">
    <div id="panel-hdr">
      <button class="hdr-btn" id="btn-close">✕</button>
    </div>
    <div id="panel-scroll">
      <div class="psec">AGENTS</div>
      <div id="alist"></div>
      <div class="psec">THREADS</div>
      <div id="tlist"></div>
    </div>
  </div>
  <div id="panel-bg"></div>
</div>

<div id="notif-bar">✓ done</div>

<script>
const $ = id => document.getElementById(id);
const msgs = $('msgs'), inp = $('inp'), sendBtn = $('send');

let agent  = localStorage.getItem('px_agent')  || '';
let thread = localStorage.getItem('px_thread') || '';
let busy = false;

// stream state
let streamEl = null, streamText = '';
let inThink = false, thinkBuf = '', thinkEl = null;
const pendingTools = new Map();

const esc = s => String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');

function renderMd(src) {
  const inl = s => {
    s = s.replace(/`([^`]+)`/g, (_,c)=>'<code>'+esc(c)+'</code>');
    s = s.replace(/\*\*([^*\n]+)\*\*/g,'<strong>$1</strong>');
    s = s.replace(/\*([^*\n]+)\*/g,'<em>$1</em>');
    return s;
  };
  const lines = src.split('\n');
  let out = '', inCode = false, codeBuf = '';
  let inUl = false, inOl = false;
  const closeList = () => {
    if (inUl){out+='</ul>';inUl=false;}
    if (inOl){out+='</ol>';inOl=false;}
  };
  for (const raw of lines) {
    if (!inCode && raw.startsWith('```')) { closeList(); inCode=true; codeBuf=''; continue; }
    if (inCode) { if (raw.startsWith('```')){out+='<pre><code>'+esc(codeBuf.replace(/\n$/,''))+'</code></pre>';inCode=false;}else codeBuf+=raw+'\n'; continue; }
    const hm = raw.match(/^(#{1,3}) (.*)/);
    if (hm) { closeList(); const t=['h3','h4','h5'][hm[1].length-1]||'h5'; out+=`<${t}>${inl(esc(hm[2]))}</${t}>`; continue; }
    const ulm = raw.match(/^[\*\-] (.*)/);
    if (ulm) { if(!inUl){closeList();out+='<ul>';inUl=true;} out+='<li>'+inl(esc(ulm[1]))+'</li>'; continue; }
    const olm = raw.match(/^\d+\. (.*)/);
    if (olm) { if(!inOl){closeList();out+='<ol>';inOl=true;} out+='<li>'+inl(esc(olm[1]))+'</li>'; continue; }
    closeList();
    out += raw.trim()==='' ? '<br>' : '<p>'+inl(esc(raw))+'</p>';
  }
  if (inCode) out+='<pre><code>'+esc(codeBuf)+'</code></pre>';
  closeList();
  return out;
}

// ── init ──────────────────────────────────────────────────────────────────────

async function init() {
  updateTitle();
  askNotifyPerm();
  const agents = await fetch('/agents').then(r => r.json()).catch(() => []);
  if (!agent && agents.length > 0) agent = agents[0];
  localStorage.setItem('px_agent', agent);
  renderAgents(agents);
  await refreshThreads();
  if (thread) await loadHistory();
  inp.focus();
}

// ── agents ────────────────────────────────────────────────────────────────────

function renderAgents(list) {
  const el = $('alist'); el.innerHTML = '';
  list.forEach(a => {
    const b = document.createElement('button');
    b.className = 'tb agent-btn' + (a === agent ? ' on' : '');
    b.textContent = a;
    b.onclick = () => switchAgent(a);
    el.appendChild(b);
  });
}

async function switchAgent(name) {
  agent = name;
  thread = '';
  localStorage.setItem('px_agent', name);
  localStorage.removeItem('px_thread');
  updateTitle();
  msgs.innerHTML = '';
  document.querySelectorAll('#alist .agent-btn').forEach(b => {
    b.classList.toggle('on', b.textContent.trim() === name);
  });
  await refreshThreads();
}

// ── threads ───────────────────────────────────────────────────────────────────

async function refreshThreads() {
  if (!agent) return;
  const list = await fetch('/agents/' + encodeURIComponent(agent) + '/threads')
    .then(r => r.json()).catch(() => []);
  renderThreads(list);
  if (!thread && list.length > 0) {
    thread = list[list.length - 1];
    localStorage.setItem('px_thread', thread);
    updateTitle();
  }
}

function renderThreads(list) {
  const el = $('tlist'); el.innerHTML = '';
  // always show current thread even if not yet on disk
  const all = thread && !list.includes(thread) ? [thread, ...list] : list;
  [...all].reverse().forEach(t => {
    const b = document.createElement('button');
    b.className = 'tb' + (t === thread ? ' on' : '');
    b.textContent = t;
    b.onclick = () => switchThread(t);
    el.appendChild(b);
  });
}

async function switchThread(name) {
  thread = name;
  localStorage.setItem('px_thread', name);
  updateTitle();
  closePanel();
  msgs.innerHTML = '';
  await loadHistory();
}

function updateTitle() {
  const parts = [agent, thread].filter(Boolean);
  $('hdr-title').textContent = parts.length ? 'PLEXUS · ' + parts.join(' / ') : 'PLEXUS';
}

async function newThread() {
  if (!agent) { alert('Select an agent first'); return; }
  const name = prompt('Thread name:');
  if (!name || !name.trim()) return;
  thread = name.trim();
  localStorage.setItem('px_thread', thread);
  updateTitle();
  msgs.innerHTML = '';
  renderThreads([]);  // renderThreads will inject current thread automatically
}

// ── history ───────────────────────────────────────────────────────────────────

async function loadHistory() {
  msgs.innerHTML = '';
  if (!agent || !thread) return;
  const hist = await fetch(
    '/agents/' + encodeURIComponent(agent) + '/history/' + encodeURIComponent(thread)
  ).then(r => r.json()).catch(() => []);
  hist.forEach(m => {
    const text = stripTimestamp(m.content);
    mkMsg(m.role, text);
  });
  scrollBot();
}

function stripTimestamp(s) {
  // Remove leading [HH:MM:SS] or [HH:MM:SS +Nms] prefix added by TimestampEnricher
  return s.replace(/^\[\d{2}:\d{2}:\d{2}[^\]]*\]\n/, '');
}

// ── send ──────────────────────────────────────────────────────────────────────

async function send() {
  const task = inp.value.trim();
  if (!task || busy) return;
  if (!agent) { alert('Select an agent first'); return; }
  if (!thread) { alert('Create a thread first (＋)'); return; }

  inp.value = ''; autosize(); setBusy(true);
  resetStream();

  mkMsg('user', task);

  try {
    const res = await fetch('/run', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({agent, task, thread})
    });
    if (!res.ok) { mkMsg('assistant', 'error ' + res.status); return; }

    const reader = res.body.getReader();
    const dec = new TextDecoder();
    let buf = '';
    while (true) {
      const {value, done} = await reader.read();
      if (done) break;
      buf += dec.decode(value, {stream: true});
      const lines = buf.split('\n'); buf = lines.pop();
      for (const line of lines) {
        if (!line.startsWith('data: ')) continue;
        let ev; try { ev = JSON.parse(line.slice(6)); } catch { continue; }
        if      (ev.type === 'text')       handleText(ev.content);
        else if (ev.type === 'tool_start') handleTool(ev.name, ev.args);
        else if (ev.type === 'tool_end')   handleToolEnd(ev.name);
        else if (ev.type === 'done')       handleDone();
      }
    }
  } catch(e) { mkMsg('assistant', 'error: ' + e.message); }

  finishStream();
  setBusy(false);
  inp.focus();
}

// ── streaming ─────────────────────────────────────────────────────────────────

function resetStream() {
  streamEl = null; streamText = '';
  inThink = false; thinkBuf = ''; thinkEl = null;
  pendingTools.clear();
}

function handleText(chunk) {
  let rest = chunk;
  while (rest.length > 0) {
    if (!inThink) {
      const ts = rest.indexOf('<think>');
      if (ts >= 0) {
        if (ts > 0) appendToStream(rest.slice(0, ts));
        inThink = true; thinkBuf = '';
        thinkEl = mkThink();
        rest = rest.slice(ts + 7);
      } else {
        appendToStream(rest); rest = '';
      }
    } else {
      const te = rest.indexOf('</think>');
      if (te >= 0) {
        thinkBuf += rest.slice(0, te);
        thinkEl.querySelector('.tc').textContent = thinkBuf;
        inThink = false; thinkBuf = '';
        rest = rest.slice(te + 8).replace(/^\n/, '');
        // collapse think block after closing tag
        if (thinkEl) thinkEl.open = false;
        thinkEl = null;
      } else {
        thinkBuf += rest;
        thinkEl.querySelector('.tc').textContent = thinkBuf;
        rest = '';
      }
    }
  }
  scrollBot();
}

function appendToStream(text) {
  streamText += text;
  if (!streamEl) {
    streamEl = document.createElement('div');
    streamEl.className = 'msg assistant streaming';
    msgs.appendChild(streamEl);
  }
  streamEl.textContent = streamText;
  scrollBot();
}

function mkThink() {
  const d = document.createElement('details');
  d.className = 'think'; d.open = true;
  d.innerHTML = '<summary>thinking</summary><div class="tc"></div>';
  msgs.appendChild(d); scrollBot();
  return d;
}

function handleTool(name, args) {
  const el = document.createElement('div');
  el.className = 'msg tool running';
  const preview = (args||'').length > 100 ? args.slice(0,100)+'…' : (args||'');
  el.innerHTML = '<span class="ti">⚙</span><span class="tn">'+esc(name)+'</span><span class="ta">'+esc(preview)+'</span>';
  msgs.appendChild(el); scrollBot();
  if (!pendingTools.has(name)) pendingTools.set(name, []);
  pendingTools.get(name).push(el);
  if (streamEl) { streamEl.classList.remove('streaming'); streamEl = null; streamText = ''; }
}

function handleToolEnd(name) {
  const stack = pendingTools.get(name);
  if (stack && stack.length) {
    const el = stack.shift();
    el.classList.remove('running'); el.classList.add('done');
    el.querySelector('.ti').textContent = '✓';
  }
}

function handleDone() { notify(); }

function finishStream() {
  if (streamEl) {
    streamEl.classList.remove('streaming');
    if (streamText.trim()) streamEl.innerHTML = renderMd(streamText);
  }
  resetStream();
}

// ── helpers ───────────────────────────────────────────────────────────────────

function mkMsg(role, text) {
  const el = document.createElement('div');
  el.className = 'msg ' + role;
  if (role === 'assistant') el.innerHTML = renderMd(text);
  else el.textContent = text;
  msgs.appendChild(el); scrollBot();
  return el;
}

function scrollBot() { msgs.scrollTop = msgs.scrollHeight; }
function setBusy(b) { busy = b; sendBtn.disabled = b; }
function autosize() { inp.style.height='auto'; inp.style.height=Math.min(inp.scrollHeight,120)+'px'; }
function openPanel() { $('panel').classList.add('open'); refreshThreads(); fetch('/agents').then(r=>r.json()).catch(()=>[]).then(renderAgents); }
function closePanel() { $('panel').classList.remove('open'); }

// ── notifications ─────────────────────────────────────────────────────────────

function askNotifyPerm() {
  if ('Notification' in window && Notification.permission === 'default')
    Notification.requestPermission();
}

function notify() {
  if (navigator.vibrate) navigator.vibrate([60, 30, 60]);
  // in-page toast
  const bar = $('notif-bar');
  bar.style.display = 'block';
  clearTimeout(bar._t);
  bar._t = setTimeout(() => bar.style.display = 'none', 2500);
  // system notification when tab is hidden
  if (document.hidden && Notification.permission === 'granted')
    new Notification('PLEXUS', {body: thread, tag: 'px', renotify: true});
}

// ── listeners ─────────────────────────────────────────────────────────────────

$('btn-menu').onclick = openPanel;
$('btn-close').onclick = closePanel;
$('panel-bg').onclick = closePanel;
$('btn-new').onclick = newThread;
$('send').onclick = send;
inp.oninput = autosize;
inp.onkeydown = e => { if (e.key==='Enter'&&!e.shiftKey){e.preventDefault();send();} };

init();
</script>
</body>
</html>"#;
