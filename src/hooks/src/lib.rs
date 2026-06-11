use anyhow::Result;
use futures::future::join_all;
use proto::Config;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

mod builtin;
mod cmd;

// ── Lifecycle hooks ───────────────────────────────────────────────────────────

/// Run all scripts in on_start/ before the agent loop.
/// Scripts receive {"task": "..."} on stdin, stdout ignored.
pub async fn run_pre(cfg: &Config, task: &str) {
    let dir = cfg.hooks_dir().join("on_start");
    run_dir(&dir, &serde_json::json!({"task": task}).to_string()).await;
}

/// Run all scripts in on_done/ after the agent loop completes.
/// Scripts receive {"task": "...", "output": "...", "tool_calls": N} on stdin.
pub async fn run_post(cfg: &Config, task: &str, output: &str, tool_calls: usize) {
    let dir = cfg.hooks_dir().join("on_done");
    let payload = serde_json::json!({
        "task": task,
        "output": output,
        "tool_calls": tool_calls,
    }).to_string();
    run_dir(&dir, &payload).await;
}

// ── Message enrichment ────────────────────────────────────────────────────────

/// Enrich message content before it is permanently written to history.
///
/// Built-in layer: each BuiltinEnricher in builtin::all() runs in order.
/// External layer: scripts in on_message/ run as a sequential pipeline —
///   each receives current content on stdin, stdout replaces content.
pub async fn enrich(content: &str, elapsed_ms: Option<u64>, cfg: &Config) -> String {
    // Built-in enrichers
    let mut result = content.to_string();
    for enricher in builtin::all() {
        result = enricher.enrich(&result, elapsed_ms);
    }

    // External pipeline — scripts sorted by filename, run sequentially
    let dir = cfg.hooks_dir().join("on_message");
    if dir.exists() {
        let mut scripts: Vec<_> = std::fs::read_dir(&dir)
            .into_iter().flatten().flatten()
            .filter(|e| !e.path().is_dir() && cmd::is_executable(&e.path()))
            .collect();
        scripts.sort_by_key(|e| e.file_name());

        for entry in scripts {
            match exec_capture(&entry.path(), &result).await {
                Ok(out) if !out.is_empty() => result = out,
                Err(err) => eprintln!(
                    "[hook/on_message/{}] {err}",
                    entry.path().file_name().unwrap_or_default().to_string_lossy()
                ),
                _ => {}
            }
        }
    }

    result
}

// ── Internal helpers ──────────────────────────────────────────────────────────

async fn run_dir(dir: &Path, payload: &str) {
    if !dir.exists() { return; }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let label = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();

    let handles: Vec<_> = rd.flatten()
        .filter(|e| !e.path().is_dir() && cmd::is_executable(&e.path()))
        .map(|e| {
            let path = e.path();
            let payload = payload.to_string();
            let label = label.clone();
            tokio::spawn(async move {
                if let Err(err) = exec(&path, &payload).await {
                    eprintln!("[hook/{label}/{}] {err}",
                        path.file_name().unwrap_or_default().to_string_lossy());
                }
            })
        })
        .collect();

    join_all(handles).await;
}

async fn exec(path: &Path, payload: &str) -> Result<()> {
    let mut child = cmd::build_cmd(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(payload.as_bytes()).await.ok();
    }
    child.wait().await?;
    Ok(())
}

async fn exec_capture(path: &Path, input: &str) -> Result<String> {
    let mut child = cmd::build_cmd(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).await.ok();
    }
    let output = child.wait_with_output().await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
}
