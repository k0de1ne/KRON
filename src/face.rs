//! The person's interface is a web app the agent writes and keeps changing.
//! There is no component catalog and no prewritten screen: files in
//! `~/.kron/ui/` are served by the hub at `/`, and the agent decides what they
//! are (a chat, a kanban board, a graph, a settings page) and how they talk to it.
//!
//! kron keeps only a kernel the agent cannot change:
//! - every change of the interface is a git commit, any one can be restored;
//! - the rescue page `/kron/` works even when the interface is broken or empty;
//! - locks (`lock`): calls the person closed with their password.

use crate::agent::{Access, Agent, Executor, Extra};
use crate::config::Config;
use crate::store::Store;
use crate::turn::TurnReq;
use crate::types::{Message, Sink, Tool};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub fn dir(cfg: &Config) -> PathBuf {
    let d = cfg.path("ui");
    std::fs::create_dir_all(&d).ok();
    d
}

pub fn data_dir(cfg: &Config) -> PathBuf {
    let d = cfg.path("ui-data");
    std::fs::create_dir_all(&d).ok();
    d
}

/// A path inside `root` from a relative one; nothing outside it.
pub fn inside(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.trim().trim_start_matches("./");
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
        bail!("'{rel}': use a path relative to the interface folder, without '..'");
    }
    Ok(root.join(p))
}

/// Writing an interface needs a model that handles tools and long instructions;
/// small local models answer in text instead.
pub fn capable(cfg: &Config, req: &TurnReq) -> bool {
    let agent = req.agent.as_deref().and_then(|a| cfg.agents.get(a));
    let name =
        req.provider.clone().or_else(|| agent.and_then(|a| a.provider.clone())).or_else(|| cfg.default_provider().ok());
    name.and_then(|n| cfg.provider(&n).ok().map(|p| p.kind != "local")).unwrap_or(false)
}

/// One turn of the agent that owns the interface.
pub async fn turn(
    cfg: &Arc<Config>,
    store: &Store,
    req: &TurnReq,
    exec: Arc<dyn Executor>,
    sink: &dyn Sink,
    history: Vec<Message>,
) -> Result<()> {
    let ui = dir(cfg);
    let tools = Arc::new(UiTools { dir: ui.clone(), data: data_dir(cfg), locks: crate::lock::Locks::new(cfg) });
    let mut agent = Agent::new(
        cfg,
        req.agent.as_deref(),
        req.provider.as_deref(),
        req.model.as_deref(),
        req.effort.as_deref(),
        exec,
    )?;
    agent.access = Access::parse(&req.access);
    agent.extra = Some(tools);
    let thread = req.thread.clone();
    let result = agent
        .run(history, sink, &|m| {
            if let Err(e) = store.append(&thread, m) {
                eprintln!("cannot save message: {e}");
            }
        })
        .await;
    commit(&ui, &crate::types::preview(&req.text, 72));
    result.map(|_| ())
}

struct UiTools {
    dir: PathBuf,
    data: PathBuf,
    locks: crate::lock::Locks,
}

#[async_trait::async_trait]
impl Extra for UiTools {
    fn tools(&self) -> Vec<Tool> {
        ui_tools()
    }

    fn system(&self) -> String {
        system_prompt(&self.dir)
    }

    async fn call(&self, name: &str, args: &Value, _sink: &dyn Sink) -> Option<String> {
        let out = match name {
            "ui_apply_patch" => self.patch(args),
            "ui_write_file" => self.write(args),
            "ui_read_file" => self.read(args).await,
            "ui_list_dir" => self.list(args).await,
            "ui_download" => self.download(args).await,
            "ui_data" => self.data(args),
            _ => return None,
        };
        Some(out.unwrap_or_else(|e| format!("error: {e:#}")))
    }
}

impl UiTools {
    fn ctx(&self) -> crate::tools::Ctx {
        crate::tools::Ctx { cwd: self.dir.clone(), ext_dirs: vec![] }
    }

    fn patch(&self, args: &Value) -> Result<String> {
        let input = args["input"].as_str().or(args["patch"].as_str()).unwrap_or("");
        for line in input.lines() {
            let line = line.trim();
            for head in ["*** Add File:", "*** Update File:", "*** Delete File:", "*** Move to:"] {
                if let Some(p) = line.strip_prefix(head) {
                    inside(&self.dir, p)?;
                }
            }
        }
        crate::tools::apply_patch::apply(input, &self.dir)
    }

    fn write(&self, args: &Value) -> Result<String> {
        let file = inside(&self.dir, args["path"].as_str().unwrap_or(""))?;
        let content = args["content"].as_str().unwrap_or("");
        if let Some(d) = file.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&file, content)?;
        Ok(format!("Wrote {} bytes to {}", content.len(), args["path"].as_str().unwrap_or("")))
    }

    async fn read(&self, args: &Value) -> Result<String> {
        let p = args["path"].as_str().unwrap_or("");
        inside(&self.dir, p)?;
        Ok(crate::tools::run(
            &self.ctx(),
            "read_file",
            &json!({"file_path": p, "offset": args["offset"], "limit": args["limit"]}),
        )
        .await)
    }

    async fn list(&self, args: &Value) -> Result<String> {
        let p = args["path"].as_str().unwrap_or(".");
        inside(&self.dir, p)?;
        Ok(crate::tools::run(
            &self.ctx(),
            "list_dir",
            &json!({"dir_path": p, "depth": args["depth"].as_u64().unwrap_or(3)}),
        )
        .await)
    }

    /// A library or an asset into the interface folder, so it works offline.
    async fn download(&self, args: &Value) -> Result<String> {
        let url = args["url"].as_str().unwrap_or("");
        if !url.starts_with("https://") {
            bail!("'url' must be an https:// address");
        }
        let file = inside(&self.dir, args["path"].as_str().unwrap_or(""))?;
        let resp = reqwest::get(url).await?.error_for_status()?;
        let bytes = resp.bytes().await?;
        if bytes.len() > 20_000_000 {
            bail!("too large ({} bytes)", bytes.len());
        }
        if let Some(d) = file.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&file, &bytes)?;
        Ok(format!("Saved {} bytes to {}", bytes.len(), args["path"].as_str().unwrap_or("")))
    }

    /// What the interface stored (`/api/ui/data`). Locked keys stay closed to the agent too.
    fn data(&self, args: &Value) -> Result<String> {
        let key = args["key"].as_str().unwrap_or("");
        if key.is_empty() {
            let keys = data_keys(&self.data);
            return Ok(if keys.is_empty() { "(no data yet)".into() } else { keys.join("\n") });
        }
        if self.locks.find("GET", &format!("/api/ui/data/{key}")).is_some() {
            bail!("the person locked '{key}': only they can open it");
        }
        let text =
            std::fs::read_to_string(data_file(&self.data, key)?).map_err(|_| anyhow::anyhow!("no data '{key}'"))?;
        Ok(crate::types::preview(&text, 60_000))
    }
}

pub fn data_file(dir: &Path, key: &str) -> Result<PathBuf> {
    if key.is_empty()
        || key.len() > 100
        || !key.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        || key.starts_with('.')
    {
        bail!("bad data key '{key}' (letters, digits, - _ .)");
    }
    Ok(dir.join(format!("{key}.json")))
}

pub fn data_keys(dir: &Path) -> Vec<String> {
    let mut keys: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".json").map(String::from))
        .collect();
    keys.sort();
    keys
}

// ── Versions: git in the interface folder ─────────────────────────────────────

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// Commit whatever changed in the interface. Without git there are no versions.
pub fn commit(dir: &Path, message: &str) {
    if !dir.join(".git").exists() {
        if git(dir, &["init", "-q"]).is_none() {
            return;
        }
        git(dir, &["config", "user.name", "kron"]);
        git(dir, &["config", "user.email", "kron@localhost"]);
    }
    git(dir, &["add", "-A"]);
    if git(dir, &["diff", "--cached", "--quiet"]).is_none() {
        let msg = if message.trim().is_empty() { "change" } else { message };
        git(dir, &["commit", "-q", "-m", msg]);
    }
}

/// Newest first: {id, ts (ms), message}.
pub fn versions(dir: &Path) -> Vec<Value> {
    git(dir, &["log", "-n", "100", "--format=%H%x09%ct%x09%s"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let (id, ts, msg) = (it.next()?, it.next()?, it.next().unwrap_or(""));
            Some(json!({"id": id, "ts": ts.parse::<i64>().ok()? * 1000, "message": msg}))
        })
        .collect()
}

/// Make the interface what it was at `id`, as a new version.
pub fn restore(dir: &Path, id: &str) -> Result<()> {
    if id.len() < 7 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("bad version id");
    }
    commit(dir, "before restoring");
    git(dir, &["cat-file", "-e", &format!("{id}^{{commit}}")]).ok_or_else(|| anyhow::anyhow!("no version {id}"))?;
    git(dir, &["rm", "-r", "-q", "--cached", "."]);
    for f in git(dir, &["ls-files", "--others"]).unwrap_or_default().lines() {
        std::fs::remove_file(dir.join(f)).ok();
    }
    git(dir, &["checkout", id, "--", "."]).ok_or_else(|| anyhow::anyhow!("cannot restore {id}"))?;
    commit(dir, &format!("restored {}", &id[..7]));
    Ok(())
}

// ── What the agent is told ────────────────────────────────────────────────────

fn ui_tools() -> Vec<Tool> {
    let t = |name: &str, description: &str, parameters: Value| Tool {
        name: name.into(),
        description: description.into(),
        parameters,
    };
    vec![
        t(
            "ui_apply_patch",
            "Edit files of the person's interface (the folder served at /) with an apply_patch patch; paths are relative to that folder.",
            json!({"type": "object", "required": ["input"], "properties": {
                "input": {"type": "string", "description": "The whole patch: *** Begin Patch ... *** End Patch"}}}),
        ),
        t(
            "ui_write_file",
            "Create or overwrite a file of the person's interface (path relative to its folder).",
            json!({"type": "object", "required": ["path", "content"], "properties": {
                "path": {"type": "string"}, "content": {"type": "string"}}}),
        ),
        t(
            "ui_read_file",
            "Read a file of the person's interface, with line numbers.",
            json!({"type": "object", "required": ["path"], "properties": {
                "path": {"type": "string"}, "offset": {"type": "number"}, "limit": {"type": "number"}}}),
        ),
        t(
            "ui_list_dir",
            "List the person's interface folder (or a folder in it).",
            json!({"type": "object", "properties": {"path": {"type": "string"}, "depth": {"type": "number"}}}),
        ),
        t(
            "ui_download",
            "Download a file (a library, a font, an image) from an https URL into the interface folder, so the interface works without the internet.",
            json!({"type": "object", "required": ["url", "path"], "properties": {
                "url": {"type": "string"}, "path": {"type": "string"}}}),
        ),
        t(
            "ui_data",
            "Read what the interface saved with /api/ui/data (no key: list the keys).",
            json!({"type": "object", "properties": {"key": {"type": "string"}}}),
        ),
    ]
}

fn system_prompt(dir: &Path) -> String {
    let files = crate::tools::fs::list_dir(dir, 2, 1, 80).unwrap_or_default();
    let empty = !dir.join("index.html").exists();
    format!(
        "{}\n<interface_folder empty=\"{empty}\">\n{}\n</interface_folder>",
        include_str!("prompts/face.md"),
        crate::types::preview(&files, 6000)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Quiet;
    impl Sink for Quiet {
        fn emit(&self, _: crate::types::Event) {}
        fn ask(&self, _: crate::types::Ask) -> Option<tokio::sync::oneshot::Receiver<String>> {
            None
        }
    }

    #[test]
    fn paths_stay_inside() {
        let root = Path::new("/r");
        assert_eq!(inside(root, "a/b.js").unwrap(), Path::new("/r/a/b.js"));
        assert_eq!(inside(root, "./index.html").unwrap(), Path::new("/r/index.html"));
        assert!(inside(root, "../x").is_err());
        assert!(inside(root, "/etc/passwd").is_err());
        assert!(inside(root, "a/../../x").is_err());
        assert!(data_file(root, "notes").is_ok());
        assert!(data_file(root, "../x").is_err());
    }

    #[tokio::test]
    async fn tools_and_versions() {
        let base = std::env::temp_dir().join(format!("kron-face-{}", crate::types::new_id()));
        let t = UiTools {
            dir: base.join("ui"),
            data: base.join("data"),
            locks: crate::lock::Locks::at(base.join("locks.json")),
        };
        std::fs::create_dir_all(&t.dir).unwrap();
        std::fs::create_dir_all(&t.data).unwrap();
        let sink = Quiet;
        let out =
            t.call("ui_write_file", &json!({"path": "index.html", "content": "<p>one</p>"}), &sink).await.unwrap();
        assert!(out.starts_with("Wrote"), "{out}");
        let out = t.call("ui_write_file", &json!({"path": "../evil", "content": "x"}), &sink).await.unwrap();
        assert!(out.starts_with("error"), "{out}");
        let bad = "*** Begin Patch\n*** Add File: /tmp/evil\n+x\n*** End Patch";
        assert!(t.call("ui_apply_patch", &json!({"input": bad}), &sink).await.unwrap().starts_with("error"));
        let patch = "*** Begin Patch\n*** Update File: index.html\n@@\n-<p>one</p>\n+<p>two</p>\n*** End Patch";
        let out = t.call("ui_apply_patch", &json!({"input": patch}), &sink).await.unwrap();
        assert!(!out.starts_with("error"), "{out}");
        assert!(t.call("ui_read_file", &json!({"path": "index.html"}), &sink).await.unwrap().contains("two"));

        std::fs::write(t.data.join("tasks.json"), "[1]").unwrap();
        std::fs::write(t.data.join("vault.json"), "\"secret\"").unwrap();
        t.locks.add("Пароли", "GET", "/api/ui/data/vault").unwrap();
        assert_eq!(t.call("ui_data", &json!({}), &sink).await.unwrap(), "tasks\nvault");
        assert_eq!(t.call("ui_data", &json!({"key": "tasks"}), &sink).await.unwrap(), "[1]");
        assert!(t.call("ui_data", &json!({"key": "vault"}), &sink).await.unwrap().contains("locked"));

        if git(&t.dir, &["--version"]).is_some() {
            commit(&t.dir, "first");
            std::fs::write(t.dir.join("index.html"), "<p>three</p>").unwrap();
            std::fs::write(t.dir.join("extra.js"), "x").unwrap();
            commit(&t.dir, "second");
            let v = versions(&t.dir);
            assert_eq!(v.len(), 2);
            assert_eq!(v[0]["message"], "second");
            restore(&t.dir, v[1]["id"].as_str().unwrap()).unwrap();
            assert_eq!(std::fs::read_to_string(t.dir.join("index.html")).unwrap().trim(), "<p>two</p>");
            assert!(!t.dir.join("extra.js").exists());
            assert_eq!(versions(&t.dir)[0]["message"], "restored ".to_string() + &v[1]["id"].as_str().unwrap()[..7]);
        }
    }
}
