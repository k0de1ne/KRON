use anyhow::Result;
use proto::ToolManifest;
use serde_json::Value;

use super::BuiltinTool;

pub struct GrepTool;

#[async_trait::async_trait]
impl BuiltinTool for GrepTool {
    fn name(&self) -> &'static str { "grep_code" }

    fn manifest(&self) -> ToolManifest {
        ToolManifest {
            name: "grep_code".into(),
            description: "Search file contents with a pattern. Returns file paths, line numbers, and matching lines.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Pattern to search for (regex when using rg, literal otherwise)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search (default: current directory)"
                    },
                    "include": {
                        "type": "string",
                        "description": "Glob pattern to filter files, e.g. '*.rs' or '*.{ts,js}'"
                    }
                }
            }),
            timeout_secs: 15,
        }
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let pattern = args.get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'pattern' field"))?;
        let path    = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let include = args.get("include").and_then(|v| v.as_str());

        // Try rg (respects .gitignore, supports regex)
        let mut cmd = tokio::process::Command::new("rg");
        cmd.args(["--no-heading", "--line-number", "--color=never"]);
        if let Some(glob) = include {
            cmd.args(["--glob", glob]);
        }
        cmd.args([pattern, path]);

        if let Ok(out) = cmd.output().await {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            if out.status.success() || !text.is_empty() {
                return Ok(text);
            }
        }

        // Fallback: literal string search
        literal_grep(pattern, path, include).await
    }
}

async fn literal_grep(pattern: &str, root: &str, include: Option<&str>) -> Result<String> {
    let mut results = String::new();
    walk_and_grep(
        std::path::Path::new(root),
        pattern,
        include,
        &mut results,
    )?;
    Ok(results)
}

fn walk_and_grep(
    dir: &std::path::Path,
    pattern: &str,
    include: Option<&str>,
    out: &mut String,
) -> Result<()> {
    if !dir.exists() { return Ok(()); }

    if dir.is_file() {
        grep_file(dir, pattern, out);
        return Ok(());
    }

    let entries = std::fs::read_dir(dir)?;
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();

    for path in paths {
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(name, ".git" | "target" | "node_modules" | "__pycache__") {
                continue;
            }
            walk_and_grep(&path, pattern, include, out)?;
        } else if path.is_file() {
            if let Some(glob) = include {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !glob_matches(glob, name) { continue; }
            }
            grep_file(&path, pattern, out);
        }
    }
    Ok(())
}

fn grep_file(path: &std::path::Path, pattern: &str, out: &mut String) {
    let Ok(content) = std::fs::read_to_string(path) else { return };
    for (i, line) in content.lines().enumerate() {
        if line.contains(pattern) {
            out.push_str(&format!("{}:{}:{}\n", path.display(), i + 1, line));
        }
    }
}

fn glob_matches(glob: &str, name: &str) -> bool {
    // Handle simple *.ext and *.{ext1,ext2} patterns
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
    glob == name || glob == "*"
}
