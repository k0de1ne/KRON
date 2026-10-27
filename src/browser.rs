//! Web chats (ChatGPT, Gemini, ...) driven in a real Chrome/Chromium/Edge with
//! a persistent profile, over the DevTools protocol. Uses the chat limits of
//! the subscription, not the API/agent ones.
//!
//! In the Android app there is no Chrome: the app keeps a WebView for web chats
//! and passes its DevTools socket in `KRON_WEBVIEW`; kron drives that page instead.

use crate::config::{BrowserCfg, Config, SiteCfg};
use anyhow::{Context, Result, bail};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message as WsMsg};

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub enum Browser {
    Chrome {
        child: Child,
        port: u16,
    },
    /// The Android app's WebView, reached through its DevTools abstract socket.
    WebView {
        socket: String,
    },
}

/// DevTools socket of the Android app's web chat WebView, if kron runs inside the app.
fn webview_socket() -> Option<String> {
    std::env::var("KRON_WEBVIEW").ok().filter(|s| !s.is_empty())
}

/// Whether this machine can serve web chats.
pub fn available(cfg: &BrowserCfg) -> bool {
    !cfg.disabled && (find_chrome(cfg).is_some() || webview_socket().is_some())
}

pub fn find_chrome(cfg: &BrowserCfg) -> Option<PathBuf> {
    if let Some(p) = cfg.path.clone().or_else(|| std::env::var("KRON_CHROME").ok()) {
        return Some(PathBuf::from(p));
    }
    let names =
        ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome", "msedge", "microsoft-edge"];
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for n in names {
            for candidate in [dir.join(n), dir.join(format!("{n}.exe"))] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    let known = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ];
    known.iter().map(PathBuf::from).find(|p| p.is_file())
}

fn running_as_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .map(|s| s.lines().any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0")))
        .unwrap_or(false)
}

impl Browser {
    /// Chrome if there is one, else the Android app's WebView.
    pub async fn start(cfg: &BrowserCfg, profile: &Path, headless: bool) -> Result<Browser> {
        if find_chrome(cfg).is_none()
            && let Some(socket) = webview_socket()
        {
            return Ok(Browser::WebView { socket });
        }
        Browser::launch(cfg, profile, headless).await
    }

    pub async fn launch(cfg: &BrowserCfg, profile: &Path, headless: bool) -> Result<Browser> {
        let exe = find_chrome(cfg).context("no Chrome/Chromium/Edge found; set [browser] path in config")?;
        std::fs::create_dir_all(profile)?;
        let port_file = profile.join("DevToolsActivePort");
        std::fs::remove_file(&port_file).ok();
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile.display()))
            .args(["--no-first-run", "--no-default-browser-check", "--disable-background-networking"])
            .arg("--disable-blink-features=AutomationControlled")
            .args(&cfg.args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if headless {
            cmd.arg("--headless=new");
        }
        if running_as_root() {
            cmd.arg("--no-sandbox");
        }
        cmd.arg("about:blank");
        let child = cmd.spawn().with_context(|| format!("cannot start {}", exe.display()))?;
        for _ in 0..200 {
            if let Some(port) =
                std::fs::read_to_string(&port_file).ok().and_then(|s| s.lines().next()?.trim().parse().ok())
            {
                return Ok(Browser::Chrome { child, port });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!("browser did not open a DevTools port (is another instance using the profile?)")
    }

    pub async fn new_page(&self) -> Result<Page> {
        let (port, socket) = match self {
            Browser::Chrome { port, .. } => (*port, None),
            Browser::WebView { socket } => (0, Some(socket.clone())),
        };
        if let Some(socket) = socket {
            // The WebView has a fixed set of pages: the app's own UI on localhost and the web chat one.
            let list: Value = serde_json::from_slice(&webview_get(&socket, "/json/list").await?)?;
            let id = list
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| t["type"] == "page")
                .find(|t| {
                    let url = t["url"].as_str().unwrap_or("");
                    !url.starts_with("http://127.0.0.1") && !url.starts_with("http://localhost")
                })
                .and_then(|t| t["id"].as_str())
                .context("the app's web chat view is not running; reopen kron")?
                .to_string();
            let stream: Box<dyn Io> = Box::new(connect_abstract(&socket).await?);
            let (ws, _) = tokio_tungstenite::client_async(format!("ws://localhost/devtools/page/{id}"), stream).await?;
            let mut page = Page { ws, next: 0, target: id, port, webview: true };
            page.call("Emulation.setFocusEmulationEnabled", json!({"enabled": true})).await.ok();
            return Ok(page);
        }
        let url = format!("http://127.0.0.1:{port}/json/new?about:blank");
        let v: Value = local_http().put(&url).send().await?.json().await?;
        let ws = v["webSocketDebuggerUrl"].as_str().context("no page websocket")?;
        let stream: Box<dyn Io> = Box::new(TcpStream::connect(("127.0.0.1", port)).await?);
        let (ws, _) = tokio_tungstenite::client_async(ws, stream).await?;
        let mut page = Page { ws, next: 0, target: v["id"].as_str().unwrap_or("").into(), port, webview: false };
        page.call("Emulation.setFocusEmulationEnabled", json!({"enabled": true})).await.ok();
        // Headless Chrome announces itself in the user agent; sites block that.
        let ua = page.call("Browser.getVersion", json!({})).await?["userAgent"]
            .as_str()
            .unwrap_or("")
            .replace("HeadlessChrome", "Chrome");
        page.call("Network.setUserAgentOverride", json!({"userAgent": ua})).await.ok();
        Ok(page)
    }

    /// Close gracefully so the profile (cookies) is flushed to disk.
    pub async fn close(self) {
        let Browser::Chrome { mut child, port } = self else { return };
        let url = format!("http://127.0.0.1:{port}/json/version");
        if let Ok(v) = async { local_http().get(&url).send().await?.json::<Value>().await }.await
            && let Some(ws) = v["webSocketDebuggerUrl"].as_str()
            && let Ok((mut ws, _)) = tokio_tungstenite::connect_async(ws).await
        {
            ws.send(WsMsg::Text(json!({"id": 1, "method": "Browser.close"}).to_string())).await.ok();
        }
        tokio::time::timeout(Duration::from_secs(5), child.wait()).await.ok();
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
async fn connect_abstract(name: &str) -> Result<tokio::net::UnixStream> {
    #[cfg(target_os = "android")]
    use std::os::android::net::SocketAddrExt;
    #[cfg(target_os = "linux")]
    use std::os::linux::net::SocketAddrExt;
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
    let s = std::os::unix::net::UnixStream::connect_addr(&addr)
        .with_context(|| format!("cannot reach the app's web chat view (@{name})"))?;
    s.set_nonblocking(true)?;
    Ok(tokio::net::UnixStream::from_std(s)?)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
async fn connect_abstract(_name: &str) -> Result<TcpStream> {
    bail!("WebView web chats only work in the Android app")
}

/// GET on the WebView's DevTools HTTP endpoint; returns the body.
async fn webview_get(socket: &str, path: &str) -> Result<Vec<u8>> {
    let mut s = connect_abstract(socket).await?;
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes()).await?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n =
            tokio::time::timeout(Duration::from_secs(5), s.read(&mut chunk)).await.context("DevTools timed out")??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        // The server may keep the connection open; stop once Content-Length is in.
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let len = head.lines().find_map(|l| l.strip_prefix("content-length:")?.trim().parse::<usize>().ok());
            if len.is_some_and(|len| buf.len() >= end + 4 + len) {
                break;
            }
        }
    }
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n").context("bad DevTools response")?;
    Ok(buf.split_off(end + 4))
}

pub struct Page {
    ws: WebSocketStream<Box<dyn Io>>,
    next: u64,
    target: String,
    port: u16,
    webview: bool,
}

impl Page {
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.ws.send(WsMsg::Text(json!({"id": id, "method": method, "params": params}).to_string())).await?;
        while let Some(msg) = self.ws.next().await {
            let WsMsg::Text(t) = msg? else { continue };
            let v: Value = serde_json::from_str(&t)?;
            if v["id"].as_u64() == Some(id) {
                if let Some(e) = v.get("error") {
                    bail!("{method}: {e}");
                }
                return Ok(v["result"].clone());
            }
        }
        bail!("browser connection closed")
    }

    pub async fn eval(&mut self, expr: &str) -> Result<Value> {
        let r = self
            .call("Runtime.evaluate", json!({"expression": expr, "returnByValue": true, "awaitPromise": true}))
            .await?;
        if let Some(e) = r.get("exceptionDetails") {
            bail!("script error: {e}");
        }
        Ok(r["result"]["value"].clone())
    }

    pub async fn goto(&mut self, url: &str) -> Result<()> {
        self.call("Page.navigate", json!({"url": url})).await?;
        Ok(())
    }

    pub async fn close(self) {
        if self.webview {
            return; // the app owns its one web chat page
        }
        let url = format!("http://127.0.0.1:{}/json/close/{}", self.port, self.target);
        local_http().get(url).send().await.ok();
    }
}

/// DevTools lives on localhost; never send it through a proxy.
fn local_http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().expect("http client")
}

fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

/// Send one prompt and wait for the full answer. `url` continues an existing
/// conversation. Returns (answer, conversation url).
pub async fn ask(
    page: &mut Page,
    site: &SiteCfg,
    prompt: &str,
    url: Option<&str>,
    timeout: Duration,
) -> Result<(String, String)> {
    page.goto(url.unwrap_or(&site.url)).await?;
    let (input, send, response, stop) = (js(&site.input), js(&site.send), js(&site.response), js(&site.stop));

    let mut ready = false;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if page.eval(&format!("!!document.querySelector({input})")).await.unwrap_or(Value::Null) == true {
            ready = true;
            break;
        }
    }
    if !ready {
        let href = page.eval("location.href").await.unwrap_or_default();
        if page.webview {
            bail!("chat input not found on {href} — не выполнен вход? Войдите: ☰ → Настройки → Веб-чаты");
        }
        bail!("chat input not found on {href} — not logged in? Run `kron browser login <site>`");
    }
    // Let a restored conversation render before counting replies.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let before = page.eval(&format!("document.querySelectorAll({response}).length")).await?.as_u64().unwrap_or(0);

    page.eval(&format!("document.querySelector({input}).focus()")).await?;
    page.call("Input.insertText", json!({"text": prompt})).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let clicked = page
        .eval(&format!("(()=>{{const b=document.querySelector({send}); if(b && !b.disabled){{b.click(); return true}} return false}})()"))
        .await?;
    if clicked != true {
        for kind in ["keyDown", "keyUp"] {
            page.call(
                "Input.dispatchKeyEvent",
                json!({"type": kind, "key": "Enter", "code": "Enter",
                "windowsVirtualKeyCode": 13, "text": if kind == "keyDown" { "\r" } else { "" }}),
            )
            .await?;
        }
    }

    let probe = format!(
        "(()=>{{const r=[...document.querySelectorAll({response})]; return {{n:r.length, text:r.length?r[r.length-1].innerText:'', busy:!!document.querySelector({stop}), href:location.href}}}})()"
    );
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = String::new();
    let mut stable = 0;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let v = page.eval(&probe).await?;
        let text = v["text"].as_str().unwrap_or("").trim().to_string();
        let done = v["n"].as_u64().unwrap_or(0) > before && v["busy"] != true && !text.is_empty();
        stable = if done && text == last { stable + 1 } else { 0 };
        last = text;
        if stable >= 2 {
            return Ok((last, v["href"].as_str().unwrap_or("").to_string()));
        }
    }
    bail!("no complete answer within {}s", timeout.as_secs())
}

// ── Process-wide browser for serving web chats ───────────────────────────────

static BROWSER: OnceLock<tokio::sync::Mutex<Option<Browser>>> = OnceLock::new();

pub fn profile_dir(cfg: &Config) -> PathBuf {
    cfg.path("browser")
}

/// One web chat turn using the shared, lazily started browser.
pub async fn web_chat(cfg: &Config, site_name: &str, prompt: &str, url: Option<&str>) -> Result<(String, String)> {
    if cfg.browser.disabled {
        bail!("web chats are disabled on this machine");
    }
    let site = cfg.sites.get(site_name).with_context(|| format!("unknown site '{site_name}'"))?.clone();
    let lock = BROWSER.get_or_init(Default::default);
    let mut guard = lock.lock().await;
    if guard.is_none() {
        *guard = Some(Browser::start(&cfg.browser, &profile_dir(cfg), cfg.browser.headless.unwrap_or(true)).await?);
    }
    let browser = guard.as_ref().unwrap();
    let mut page = match browser.new_page().await {
        Ok(p) => p,
        Err(_) => {
            // The browser died; start a fresh one.
            *guard = Some(Browser::start(&cfg.browser, &profile_dir(cfg), cfg.browser.headless.unwrap_or(true)).await?);
            guard.as_ref().unwrap().new_page().await?
        }
    };
    let result = ask(&mut page, &site, prompt, url, Duration::from_secs(600)).await;
    page.close().await;
    result
}

/// Close the shared browser (a one-shot CLI run must not leave it behind).
pub async fn shutdown() {
    if let Some(lock) = BROWSER.get()
        && let Some(b) = lock.lock().await.take()
    {
        b.close().await;
    }
}

/// Open a visible browser on the site so the user can sign in once.
pub async fn login(cfg: &Config, site_name: &str) -> Result<()> {
    let site = cfg.sites.get(site_name).with_context(|| format!("unknown site '{site_name}'"))?;
    let browser = Browser::launch(&cfg.browser, &profile_dir(cfg), false).await?;
    let mut page = browser.new_page().await?;
    page.goto(&site.url).await?;
    println!("Sign in to {site_name} in the opened window, then press Enter here.");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    page.close().await;
    browser.close().await;
    println!("Saved session in {}", profile_dir(cfg).display());
    Ok(())
}
