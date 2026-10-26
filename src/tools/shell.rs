//! Command execution: one-shot `shell` and long-running `exec_command` sessions
//! fed through `write_stdin` (Codex's unified exec, over pipes so it works the
//! same on Windows, Linux and Android).

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};

const MAX_OUTPUT: usize = 16_000;

/// The user's shell running `command`.
pub fn command(cmd: &str, cwd: &Path) -> Command {
    let mut c = if cfg!(windows) {
        let mut c = Command::new("powershell.exe");
        c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", cmd]);
        c
    } else {
        let sh = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "sh".into());
        let mut c = Command::new(sh);
        c.args(["-c", cmd]);
        c
    };
    c.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    c
}

pub async fn run(cmd: &str, cwd: &Path, timeout_ms: u64) -> Result<String> {
    let child = command(cmd, cwd).spawn().with_context(|| format!("cannot start shell in {}", cwd.display()))?;
    match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await {
        Ok(out) => {
            let out = out?;
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            Ok(format!("Exit code: {}\nOutput:\n{}", out.status.code().unwrap_or(-1), truncate(&text)))
        }
        Err(_) => Ok(format!("Command timed out after {} ms and was killed.", timeout_ms)),
    }
}

/// Keep the head and the tail of long output.
pub fn truncate(s: &str) -> String {
    if s.len() <= MAX_OUTPUT {
        return s.to_string();
    }
    let half = MAX_OUTPUT / 2;
    let mut a = half;
    while !s.is_char_boundary(a) {
        a -= 1;
    }
    let mut b = s.len() - half;
    while !s.is_char_boundary(b) {
        b += 1;
    }
    format!("{}\n[… {} bytes truncated …]\n{}", &s[..a], b - a, &s[b..])
}

// ── Sessions ──────────────────────────────────────────────────────────────────

struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    output: Arc<Mutex<Vec<u8>>>,
}

fn sessions() -> &'static tokio::sync::Mutex<HashMap<u32, Session>> {
    static S: OnceLock<tokio::sync::Mutex<HashMap<u32, Session>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

pub async fn exec_start(cmd: &str, cwd: &Path, yield_ms: u64) -> Result<String> {
    let mut child = command(cmd, cwd).stdin(Stdio::piped()).spawn()?;
    let output = Arc::new(Mutex::new(Vec::new()));
    if let Some(out) = child.stdout.take() {
        pump(out, output.clone());
    }
    if let Some(err) = child.stderr.take() {
        pump(err, output.clone());
    }
    let stdin = child.stdin.take();
    let id = {
        let mut s = sessions().lock().await;
        let id = (1..).find(|i| !s.contains_key(i)).unwrap();
        s.insert(id, Session { child, stdin, output });
        id
    };
    collect(id, yield_ms).await
}

pub async fn exec_write(id: u32, chars: &str, yield_ms: u64) -> Result<String> {
    {
        let mut s = sessions().lock().await;
        let sess = s.get_mut(&id).with_context(|| format!("no running session {id}"))?;
        if !chars.is_empty() {
            let Some(stdin) = sess.stdin.as_mut() else { bail!("session {id} stdin is closed") };
            stdin.write_all(chars.as_bytes()).await?;
            stdin.flush().await?;
        }
    }
    collect(id, yield_ms).await
}

/// Wait up to `yield_ms` (less if the process exits), then return new output.
async fn collect(id: u32, yield_ms: u64) -> Result<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(yield_ms);
    loop {
        let mut s = sessions().lock().await;
        let sess = s.get_mut(&id).context("session vanished")?;
        let exited = sess.child.try_wait()?;
        if exited.is_some() || tokio::time::Instant::now() >= deadline {
            if exited.is_some() {
                // Let the reader tasks drain what is left in the pipes.
                drop(s);
                tokio::time::sleep(Duration::from_millis(50)).await;
                s = sessions().lock().await;
            }
            let sess = s.get_mut(&id).context("session vanished")?;
            let out = String::from_utf8_lossy(&std::mem::take(&mut *sess.output.lock().unwrap())).to_string();
            return Ok(match exited {
                Some(status) => {
                    s.remove(&id);
                    format!("Process exited with code {}\nOutput:\n{}", status.code().unwrap_or(-1), truncate(&out))
                }
                None => format!("Process running with session_id {id}\nOutput:\n{}", truncate(&out)),
            });
        }
        drop(s);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn pump(mut r: impl tokio::io::AsyncRead + Unpin + Send + 'static, output: Arc<Mutex<Vec<u8>>>) {
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        while let Ok(n) = r.read(&mut buf).await {
            if n == 0 {
                break;
            }
            output.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
}
