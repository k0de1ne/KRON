use anyhow::Result;
use proto::ToolManifest;
use serde_json::Value;

use super::BuiltinTool;

pub struct ListFilesTool;

#[async_trait::async_trait]
impl BuiltinTool for ListFilesTool {
    fn name(&self) -> &'static str { "list_files" }

    fn manifest(&self) -> ToolManifest {
        ToolManifest {
            name: "list_files".into(),
            description: "List files in a directory. Use to explore project structure before reading files.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "required": [],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Root directory to list (default: current directory)"
                    },
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to filter files, e.g. '*.rs' or '**/*.toml'"
                    }
                }
            }),
            timeout_secs: 10,
        }
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let path    = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let pattern = args.get("pattern").and_then(|v| v.as_str());

        // Try rg --files (respects .gitignore)
        let mut cmd = tokio::process::Command::new("rg");
        cmd.args(["--files", "--color=never"]);
        if let Some(glob) = pattern {
            cmd.args(["--glob", glob]);
        }
        cmd.arg(path);

        if let Ok(out) = cmd.output().await {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if !text.is_empty() {
                    // Sort for stable output
                    let mut lines: Vec<&str> = text.lines().collect();
                    lines.sort();
                    return Ok(lines.join("\n"));
                }
            }
        }

        // Fallback: recursive walk
        let mut files = Vec::new();
        walk_files(std::path::Path::new(path), pattern, &mut files)?;
        files.sort();
        Ok(files.join("\n"))
    }
}

fn walk_files(
    dir: &std::path::Path,
    pattern: Option<&str>,
    out: &mut Vec<String>,
) -> Result<()> {
    if !dir.exists() { return Ok(()); }

    let entries = std::fs::read_dir(dir)?;
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();

    for path in paths {
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(name, ".git" | "target" | "node_modules" | "__pycache__") {
                continue;
            }
            walk_files(&path, pattern, out)?;
        } else if path.is_file() {
            if let Some(glob) = pattern {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !glob_matches(glob, name) { continue; }
            }
            out.push(path.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

fn glob_matches(glob: &str, name: &str) -> bool {
    if let (Some(start), Some(end)) = (glob.find('{'), glob.rfind('}')) {
        let prefix = &glob[..start];
        let exts: Vec<&str> = glob[start + 1..end].split(',').collect();
        let suffix_after = &glob[end + 1..];
        return exts.iter().any(|ext| {
            glob_matches(&format!("{}{}{}", prefix, ext, suffix_after), name)
        });
    }
    if glob.starts_with("*.") {
        return name.ends_with(&glob[1..]);
    }
    // Strip leading **/ for recursive patterns
    let base = glob.trim_start_matches("**/");
    if base.starts_with("*.") {
        return name.ends_with(&base[1..]);
    }
    glob == name || glob == "*" || glob == "**/*"
}
