use anyhow::Result;
use proto::ToolManifest;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

mod builtin;
mod cmd;

pub fn discover(tool_paths: &[PathBuf]) -> Vec<ToolManifest> {
    // Start with built-ins
    let mut map: HashMap<String, ToolManifest> = builtin::all()
        .iter()
        .map(|b| (b.name().to_string(), b.manifest()))
        .collect();

    // External tools override built-ins (same name = user's version wins)
    for tool in discover_external(tool_paths) {
        map.insert(tool.name.clone(), tool);
    }

    let mut tools: Vec<ToolManifest> = map.into_values().collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}

pub async fn run(
    tool_paths: &[PathBuf],
    name: &str,
    input: &Value,
    timeout_secs: u64,
) -> Result<String> {
    let timeout = Duration::from_secs(timeout_secs);

    // External takes priority — user can override any built-in
    if let Some(binary) = find_external(tool_paths, name) {
        let json = serde_json::to_string(input)?;
        return tokio::time::timeout(timeout, exec_external(&binary, &json))
            .await
            .map_err(|_| anyhow::anyhow!("tool '{}' timed out after {}s", name, timeout_secs))?;
    }

    // Built-in fallback
    let tools = builtin::all();
    if let Some(tool) = tools.iter().find(|b| b.name() == name) {
        return tokio::time::timeout(timeout, tool.call(input))
            .await
            .map_err(|_| anyhow::anyhow!("tool '{}' timed out after {}s", name, timeout_secs))?;
    }

    Err(anyhow::anyhow!("tool not found: {}", name))
}

fn discover_external(tool_paths: &[PathBuf]) -> Vec<ToolManifest> {
    let mut tools = Vec::new();
    for dir in tool_paths {
        if !dir.exists() { continue; }
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() { continue; }
            if path.extension().and_then(|e| e.to_str()) == Some("toml") { continue; }
            if !is_executable(&path) { continue; }

            let stem = path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();

            let manifest_path = path.with_extension("toml");
            let tool = if manifest_path.exists() {
                std::fs::read_to_string(&manifest_path).ok()
                    .and_then(|s| toml::from_str::<ToolManifest>(&s).ok())
                    .unwrap_or_else(|| ToolManifest::minimal(&stem))
            } else {
                ToolManifest::minimal(&stem)
            };
            tools.push(tool);
        }
    }
    tools
}

fn find_external(tool_paths: &[PathBuf], name: &str) -> Option<PathBuf> {
    for dir in tool_paths {
        let p = dir.join(name);
        if p.exists() && is_executable(&p) { return Some(p); }
        if cfg!(windows) {
            for ext in ["exe", "bat", "cmd", "ps1"] {
                let p = dir.join(format!("{}.{}", name, ext));
                if p.exists() { return Some(p); }
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e.to_lowercase().as_str(), "exe" | "bat" | "cmd" | "ps1"))
            .unwrap_or(false)
    }
}

async fn exec_external(path: &Path, stdin_json: &str) -> Result<String> {
    let mut child = cmd::build_cmd(path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut s) = child.stdin.take() {
        s.write_all(stdin_json.as_bytes()).await?;
    }

    let out = child.wait_with_output().await?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    if !out.status.success() && stdout.is_empty() {
        return Ok(format!("error (exit {}): {}", out.status, stderr.trim()));
    }
    Ok(if stdout.is_empty() { stderr } else { stdout })
}
