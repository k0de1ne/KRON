//! Local models: GGUF files in ~/.kron/models, served by llama.cpp's
//! llama-server. Each downloaded file shows up as a provider of kind "local";
//! the server starts on first use and stops after a while without requests,
//! so a phone does not keep gigabytes in memory for nothing.

use crate::config::Config;
use crate::llm::{Provider, Request, Response, http, openai::OpenAi};
use crate::types::Sink;
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

pub struct Model {
    pub file: &'static str,
    pub repo: &'static str,
    pub title: &'static str,
    pub note: &'static str,
    pub size: u64,
}

/// Liquid AI LFM2.5: hybrid models built for phones, with tool calling that
/// llama.cpp parses natively.
pub const CATALOG: &[Model] = &[
    Model {
        file: "LFM2.5-2.6B-Q4_K_M.gguf",
        repo: "LiquidAI/LFM2.5-2.6B-GGUF",
        title: "LFM2.5 2.6B",
        note: "лучший выбор для телефона: чат и агент с инструментами",
        size: 1_674_455_040,
    },
    Model {
        file: "LFM2.5-8B-A1B-Q4_K_M.gguf",
        repo: "LiquidAI/LFM2.5-8B-A1B-GGUF",
        title: "LFM2.5 8B-A1B",
        note: "умнее, но быстрая (активен 1B); нужно 8+ ГБ памяти",
        size: 5_155_564_768,
    },
    Model {
        file: "LFM2.5-1.2B-Instruct-Q4_K_M.gguf",
        repo: "LiquidAI/LFM2.5-1.2B-Instruct-GGUF",
        title: "LFM2.5 1.2B",
        note: "быстрый простой чат",
        size: 730_895_168,
    },
    Model {
        file: "LFM2.5-350M-Q4_K_M.gguf",
        repo: "LiquidAI/LFM2.5-350M-GGUF",
        title: "LFM2.5 350M",
        note: "крошечная: перевод, короткие ответы",
        size: 229_312_224,
    },
];

const PORT: u16 = 7701;
const IDLE: Duration = Duration::from_secs(600);

pub fn models_dir(home: &Path) -> PathBuf {
    home.join("models")
}

/// Downloaded models as (provider name, file stem): "lfm2.5-2.6b" for
/// LFM2.5-2.6B-Q4_K_M.gguf.
pub fn installed(home: &Path) -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir(models_dir(home)) else { return Vec::new() };
    let mut out: Vec<_> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let stem = name.strip_suffix(".gguf")?.to_string();
            let short = stem.rsplit_once("-Q").map(|(a, _)| a).unwrap_or(&stem).to_lowercase();
            Some((short, stem))
        })
        .collect();
    out.sort();
    out
}

// ── Downloads ─────────────────────────────────────────────────────────────────

#[derive(Clone, Default, serde::Serialize)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
    pub finished: bool,
}

fn downloads() -> &'static Mutex<HashMap<String, Progress>> {
    static D: OnceLock<Mutex<HashMap<String, Progress>>> = OnceLock::new();
    D.get_or_init(Default::default)
}

pub fn progress(file: &str) -> Option<Progress> {
    downloads().lock().unwrap().get(file).cloned()
}

/// Start downloading a catalog model in the background; `done` runs after success.
pub fn download(home: &Path, file: &str, done: impl FnOnce() + Send + 'static) -> Result<()> {
    let m = CATALOG.iter().find(|m| m.file == file).context("unknown model")?;
    {
        let mut d = downloads().lock().unwrap();
        if d.get(file).is_some_and(|p| !p.finished) {
            return Ok(());
        }
        d.insert(file.into(), Progress { total: m.size, ..Default::default() });
    }
    let dir = models_dir(home);
    let url = format!("https://huggingface.co/{}/resolve/main/{}", m.repo, m.file);
    let file = file.to_string();
    tokio::spawn(async move {
        let result = fetch(&url, &dir, &file).await;
        let mut d = downloads().lock().unwrap();
        let p = d.entry(file).or_default();
        p.finished = true;
        match result {
            Ok(()) => {
                drop(d);
                done();
            }
            Err(e) => p.error = Some(format!("{e:#}")),
        }
    });
    Ok(())
}

async fn fetch(url: &str, dir: &Path, file: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let part = dir.join(format!("{file}.part"));
    let resp = crate::llm::check(http().get(url).send().await?).await?;
    if let Some(n) = resp.content_length() {
        downloads().lock().unwrap().entry(file.into()).or_default().total = n;
    }
    let mut out = tokio::fs::File::create(&part).await?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        out.write_all(&chunk).await?;
        downloads().lock().unwrap().entry(file.into()).or_default().done += chunk.len() as u64;
    }
    out.flush().await?;
    drop(out);
    std::fs::rename(&part, dir.join(file))?;
    Ok(())
}

pub async fn remove(home: &Path, file: &str) -> Result<()> {
    let path = models_dir(home).join(file);
    if !file.ends_with(".gguf") || file.contains(['/', '\\']) {
        bail!("bad model name");
    }
    let mut s = server().lock().await;
    if s.as_ref().is_some_and(|s| s.model == path) {
        *s = None;
    }
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    downloads().lock().unwrap().remove(file);
    Ok(())
}

// ── llama-server ──────────────────────────────────────────────────────────────

struct Server {
    model: PathBuf,
    child: tokio::process::Child,
    last_used: Instant,
}

fn server() -> &'static tokio::sync::Mutex<Option<Server>> {
    static S: OnceLock<tokio::sync::Mutex<Option<Server>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// llama-server from $KRON_LLAMA_SERVER, next to kron (the Android app ships it
/// as libllama-server.so), or on PATH.
pub fn find_server() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("KRON_LLAMA_SERVER") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for name in ["libllama-server.so", "llama-server", "llama-server.exe"] {
        if dir.join(name).is_file() {
            return Some(dir.join(name));
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|d| [d.join("llama-server"), d.join("llama-server.exe")])
        .find(|p| p.is_file())
}

async fn ensure(home: &Path, model: &Path) -> Result<()> {
    let mut s = server().lock().await;
    if let Some(cur) = s.as_mut()
        && cur.model == model
        && cur.child.try_wait()?.is_none()
    {
        cur.last_used = Instant::now();
        return Ok(());
    }
    *s = None; // kill_on_drop stops a server running another model
    if !model.is_file() {
        bail!("model {} is not downloaded (Settings → Local models)", model.display());
    }
    let exe = find_server().context(
        "llama-server not found: install llama.cpp (winget install llama.cpp, brew install llama.cpp) \
         or put llama-server next to kron",
    )?;
    let log = std::fs::File::create(models_dir(home).join("llama-server.log"))?;
    let mut child = tokio::process::Command::new(&exe)
        .arg("-m")
        .arg(model)
        .args(["--host", "127.0.0.1", "--port", &PORT.to_string(), "--jinja", "-c", "16384", "--no-webui"])
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    // Loading a model takes a few seconds on a phone.
    let health = format!("http://127.0.0.1:{PORT}/health");
    for _ in 0..240 {
        if let Some(status) = child.try_wait()? {
            let tail = std::fs::read_to_string(models_dir(home).join("llama-server.log")).unwrap_or_default();
            let tail: String =
                tail.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            bail!("llama-server exited ({status}):\n{tail}");
        }
        if http().get(&health).send().await.is_ok_and(|r| r.status().is_success()) {
            *s = Some(Server { model: model.to_path_buf(), child, last_used: Instant::now() });
            spawn_idle_stop();
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("llama-server did not become ready in 2 minutes")
}

fn spawn_idle_stop() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let mut s = server().lock().await;
                if ACTIVE.load(Ordering::SeqCst) == 0 && s.as_ref().is_some_and(|s| s.last_used.elapsed() > IDLE) {
                    *s = None;
                }
            }
        });
    });
}

/// Stop the server (a one-shot CLI run must not leave it behind).
pub async fn shutdown() {
    *server().lock().await = None;
}

pub struct Local {
    home: PathBuf,
    model: PathBuf,
    api: OpenAi,
}

impl Local {
    pub fn new(cfg: &Config, p: &crate::config::ProviderCfg) -> Local {
        let file = if p.model.ends_with(".gguf") { p.model.clone() } else { format!("{}.gguf", p.model) };
        let model =
            if Path::new(&file).is_absolute() { PathBuf::from(&file) } else { models_dir(&cfg.home).join(&file) };
        let mut api = p.clone();
        api.base_url = Some(format!("http://127.0.0.1:{PORT}/v1"));
        Local { home: cfg.home.clone(), model, api: OpenAi::new(&api) }
    }
}

struct Busy;
impl Drop for Busy {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl Provider for Local {
    async fn complete(&self, req: Request<'_>, sink: &dyn Sink) -> Result<Response> {
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        let _busy = Busy;
        ensure(&self.home, &self.model).await?;
        let r = self.api.complete(req, sink).await;
        if let Some(s) = server().lock().await.as_mut() {
            s.last_used = Instant::now();
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloaded_models_become_providers() {
        let home = std::env::temp_dir().join(format!("kron-local-{}", crate::types::new_id()));
        std::fs::create_dir_all(models_dir(&home)).unwrap();
        for f in ["LFM2.5-2.6B-Q4_K_M.gguf", "LFM2.5-350M-Q4_K_M.gguf.part", "my-model.gguf"] {
            std::fs::write(models_dir(&home).join(f), "").unwrap();
        }
        assert_eq!(
            installed(&home),
            vec![("lfm2.5-2.6b".into(), "LFM2.5-2.6B-Q4_K_M".into()), ("my-model".into(), "my-model".into())]
        );
    }
}
