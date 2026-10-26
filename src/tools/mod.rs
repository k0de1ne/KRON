//! Tools the agent can call. The set follows the Codex CLI; scripts in
//! ~/.kron/tools add more (stdin: JSON arguments, stdout: result).

use crate::types::Tool;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub mod apply_patch;
pub mod fs;
pub mod shell;

/// Tools handled by the agent loop itself rather than on the machine running the tools.
pub const AGENT_SIDE: &[&str] = &["web_search", "update_plan", "spawn_agent"];

#[derive(Clone)]
pub struct Ctx {
    pub cwd: PathBuf,
    pub ext_dirs: Vec<PathBuf>,
}

fn tool(name: &str, description: &str, parameters: Value) -> Tool {
    Tool { name: name.into(), description: description.into(), parameters }
}

pub fn builtin() -> Vec<Tool> {
    vec![
        tool("shell", "Run a shell command (PowerShell on Windows, sh elsewhere) and return its output. \
            Set workdir instead of using cd.",
            json!({"type": "object", "required": ["command"], "properties": {
                "command": {"type": "string", "description": "The command line to run"},
                "workdir": {"type": "string", "description": "Working directory (default: session cwd)"},
                "timeout_ms": {"type": "number", "description": "Kill after this many ms (default 120000)"}}})),
        tool("exec_command", "Start a long-running or interactive command. Returns output collected for \
            yield_time_ms and, if still running, a session_id for write_stdin.",
            json!({"type": "object", "required": ["cmd"], "properties": {
                "cmd": {"type": "string"},
                "workdir": {"type": "string"},
                "yield_time_ms": {"type": "number", "description": "How long to wait for output (default 10000)"}}})),
        tool("write_stdin", "Write characters to a running exec_command session (empty string just polls) \
            and return new output.",
            json!({"type": "object", "required": ["session_id"], "properties": {
                "session_id": {"type": "number"},
                "chars": {"type": "string", "description": "Text to send; include \\n to submit a line"},
                "yield_time_ms": {"type": "number"}}})),
        tool("apply_patch", "Edit files with a patch. Format:\n*** Begin Patch\n*** Add File: path\n+line\n\
            *** Delete File: path\n*** Update File: path\n*** Move to: new_path (optional)\n@@ context line (optional)\n \
            unchanged\n-removed\n+added\n*** End Patch\nShow 3 lines of context around each change; \
            paths are relative to the working directory.",
            json!({"type": "object", "required": ["input"], "properties": {
                "input": {"type": "string", "description": "The whole patch"}}})),
        tool("read_file", "Read a text file with line numbers.",
            json!({"type": "object", "required": ["file_path"], "properties": {
                "file_path": {"type": "string"},
                "offset": {"type": "number", "description": "1-based line to start at"},
                "limit": {"type": "number", "description": "Max lines (default 2000)"}}})),
        tool("list_dir", "List a directory tree.",
            json!({"type": "object", "required": ["dir_path"], "properties": {
                "dir_path": {"type": "string"},
                "depth": {"type": "number", "description": "Levels to descend (default 2)"},
                "offset": {"type": "number"},
                "limit": {"type": "number"}}})),
        tool("grep_files", "Search file contents with a regex (respects .gitignore). Returns path:line: text.",
            json!({"type": "object", "required": ["pattern"], "properties": {
                "pattern": {"type": "string"},
                "include": {"type": "string", "description": "Glob filter, e.g. \"*.rs\" or \"*.{ts,tsx}\""},
                "path": {"type": "string", "description": "Directory to search (default: cwd)"},
                "limit": {"type": "number"}}})),
        tool("update_plan", "Record or update your step-by-step plan. Keep exactly one step in_progress.",
            json!({"type": "object", "required": ["plan"], "properties": {
                "explanation": {"type": "string"},
                "plan": {"type": "array", "items": {"type": "object", "required": ["step", "status"], "properties": {
                    "step": {"type": "string"},
                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}}}}}})),
        tool("web_search", "Search the web with a provider's native search and get a sourced answer.",
            json!({"type": "object", "required": ["query"], "properties": {"query": {"type": "string"}}})),
        tool("spawn_agent", "Hand a self-contained task to another agent (see the agent list in your \
            instructions) and get its final answer. Several calls run in parallel.",
            json!({"type": "object", "required": ["task"], "properties": {
                "agent": {"type": "string", "description": "Agent name (default: same as you)"},
                "task": {"type": "string"}}})),
    ]
}

/// Scripts in the extension dirs. `name.toml` next to a script describes it.
pub fn external(dirs: &[PathBuf]) -> Vec<Tool> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if !is_script(&path) {
                continue;
            }
            let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let manifest = std::fs::read_to_string(path.with_extension("toml"))
                .ok()
                .and_then(|t| toml::from_str::<Value>(&t).ok());
            let m = manifest.unwrap_or(json!({}));
            out.push(tool(
                m["name"].as_str().unwrap_or(&stem),
                m["description"].as_str().unwrap_or("External tool. Input is a JSON object."),
                m.get("parameters")
                    .cloned()
                    .unwrap_or(json!({"type": "object", "properties": {"args": {"type": "string"}}})),
            ));
        }
    }
    out
}

fn is_script(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "toml" || ext == "md" {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        matches!(ext.as_str(), "exe" | "bat" | "cmd" | "ps1")
    }
}

fn find_script(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if is_script(&p) && p.file_stem().is_some_and(|s| s == name) {
                return Some(p);
            }
        }
    }
    None
}

async fn run_script(path: &Path, args: &Value, cwd: &Path) -> anyhow::Result<String> {
    use tokio::io::AsyncWriteExt;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let mut cmd = match ext.as_str() {
        "ps1" => {
            let mut c = tokio::process::Command::new("powershell.exe");
            c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(path);
            c
        }
        "bat" | "cmd" => {
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/c").arg(path);
            c
        }
        _ => tokio::process::Command::new(path),
    };
    let mut child = cmd
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(args.to_string().as_bytes()).await?;
    }
    let out = tokio::time::timeout(std::time::Duration::from_secs(600), child.wait_with_output()).await??;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    Ok(shell::truncate(&text))
}

/// Execute a tool on this machine. Errors come back as text for the model.
pub async fn run(ctx: &Ctx, name: &str, args: &Value) -> String {
    match run_inner(ctx, name, args).await {
        Ok(s) => s,
        Err(e) => format!("error: {e:#}"),
    }
}

async fn run_inner(ctx: &Ctx, name: &str, a: &Value) -> anyhow::Result<String> {
    let s = |k: &str| a[k].as_str();
    let n = |k: &str, d: u64| a[k].as_u64().or_else(|| a[k].as_f64().map(|f| f as u64)).unwrap_or(d);
    let path = |k: &str| -> PathBuf {
        match s(k) {
            Some(p) if Path::new(p).is_absolute() => PathBuf::from(p),
            Some(p) => ctx.cwd.join(p),
            None => ctx.cwd.clone(),
        }
    };
    let need = |k: &str| s(k).ok_or_else(|| anyhow::anyhow!("missing '{k}'"));

    if let Some(script) = find_script(&ctx.ext_dirs, name) {
        return run_script(&script, a, &ctx.cwd).await;
    }
    match name {
        "shell" => {
            let command = need("command")?;
            let dir = path("workdir");
            if let Some(patch) = apply_patch::from_shell(command) {
                return apply_patch::apply(&patch, &dir);
            }
            shell::run(command, &dir, n("timeout_ms", 120_000)).await
        }
        "exec_command" => shell::exec_start(need("cmd")?, &path("workdir"), n("yield_time_ms", 10_000)).await,
        "write_stdin" => {
            let id = n("session_id", 0) as u32;
            shell::exec_write(id, s("chars").unwrap_or(""), n("yield_time_ms", 2_000)).await
        }
        "apply_patch" => {
            apply_patch::apply(s("input").or(s("patch")).ok_or_else(|| anyhow::anyhow!("missing 'input'"))?, &ctx.cwd)
        }
        "read_file" => {
            need("file_path")?;
            fs::read_file(&path("file_path"), n("offset", 1) as usize, n("limit", 2000) as usize)
        }
        "list_dir" => {
            fs::list_dir(&path("dir_path"), n("depth", 2) as usize, n("offset", 1) as usize, n("limit", 500) as usize)
        }
        "grep_files" => fs::grep(&path("path"), need("pattern")?, s("include"), n("limit", 200) as usize),
        _ => anyhow::bail!("unknown tool '{name}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_and_sessions() {
        let ctx = Ctx { cwd: std::env::temp_dir(), ext_dirs: vec![] };
        let out = run(&ctx, "shell", &json!({"command": "echo hi"})).await;
        assert!(out.starts_with("Exit code: 0") && out.contains("hi"), "{out}");

        let start = run(&ctx, "exec_command", &json!({"cmd": "read x; echo got $x", "yield_time_ms": 300})).await;
        assert!(start.contains("session_id 1"), "{start}");
        let end = run(&ctx, "write_stdin", &json!({"session_id": 1, "chars": "abc\n", "yield_time_ms": 3000})).await;
        assert!(end.contains("exited with code 0") && end.contains("got abc"), "{end}");
    }
}
