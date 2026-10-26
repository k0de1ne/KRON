//! Files on a node for the web UI: browse the agent's working folder, open
//! and download what it made, upload attachments. Same requests locally and
//! through the hub (job kind "files").

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Largest file sent to the UI in one piece.
pub const MAX_READ: u64 = 25_000_000;

/// A path from the UI: relative to `cwd`, `~` for home, absolute as seen by the agent
/// (inside the Android Linux, if there is one).
pub fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let p = p.trim();
    if p.is_empty() || p == "." {
        return cwd.to_path_buf();
    }
    let sb = crate::sandbox::get();
    if p == "~" || p.starts_with("~/") {
        let home = match sb {
            Some(sb) => sb.home(),
            None => crate::config::user_home(),
        };
        return home.join(p.trim_start_matches('~').trim_start_matches('/'));
    }
    if Path::new(p).is_absolute() {
        return match sb {
            Some(sb) => sb.host_path(Path::new(p)),
            None => PathBuf::from(p),
        };
    }
    cwd.join(p)
}

/// How the agent sees a path.
pub fn shown(p: &Path) -> String {
    let p = match crate::sandbox::get() {
        Some(sb) => sb.guest_path(p),
        None => p.to_path_buf(),
    };
    p.display().to_string()
}

/// `req`: {"op": "list" | "read" | "write" | "mkdir" | "stat", "path", "data" (base64, write)}
pub fn handle(cwd: &Path, req: &Value) -> Result<Value> {
    let path = resolve(cwd, req["path"].as_str().unwrap_or(""));
    match req["op"].as_str().unwrap_or("") {
        "stat" => {
            let m = std::fs::metadata(&path).with_context(|| format!("{} not found", shown(&path)))?;
            Ok(json!({"path": shown(&path), "dir": m.is_dir(), "size": m.len()}))
        }
        "list" => {
            let mut entries = Vec::new();
            let rd = std::fs::read_dir(&path).with_context(|| format!("cannot open {}", shown(&path)))?;
            for e in rd.flatten() {
                let Ok(m) = std::fs::metadata(e.path()) else { continue };
                let modified = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                entries.push(json!({
                    "name": e.file_name().to_string_lossy(),
                    "dir": m.is_dir(),
                    "size": if m.is_dir() { 0 } else { m.len() },
                    "modified": modified,
                }));
            }
            entries.sort_by(|a, b| {
                (b["dir"].as_bool(), a["name"].as_str().map(str::to_lowercase))
                    .cmp(&(a["dir"].as_bool(), b["name"].as_str().map(str::to_lowercase)))
            });
            let parent = path.parent().map(shown);
            Ok(json!({"path": shown(&path), "parent": parent, "entries": entries}))
        }
        "read" => {
            let m = std::fs::metadata(&path).with_context(|| format!("{} not found", shown(&path)))?;
            if m.is_dir() {
                bail!("{} is a folder", shown(&path));
            }
            if m.len() > MAX_READ {
                bail!("{} is too large to open here ({} MB)", shown(&path), m.len() / 1_000_000);
            }
            let bytes = std::fs::read(&path)?;
            Ok(json!({
                "path": shown(&path),
                "size": bytes.len(),
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            }))
        }
        "write" => {
            let data = base64::engine::general_purpose::STANDARD
                .decode(req["data"].as_str().unwrap_or(""))
                .context("bad file data")?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, &data).with_context(|| format!("cannot write {}", shown(&path)))?;
            Ok(json!({"path": shown(&path), "size": data.len()}))
        }
        "mkdir" => {
            std::fs::create_dir_all(&path).with_context(|| format!("cannot create {}", shown(&path)))?;
            Ok(json!({"path": shown(&path)}))
        }
        other => bail!("unknown files op '{other}'"),
    }
}

/// Content type for serving a file to the browser.
pub fn mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "xml" => "application/xml",
        "txt" | "md" | "csv" | "log" | "toml" | "yaml" | "yml" | "rs" | "py" | "ts" | "tsx" | "jsx" | "go" | "java"
        | "c" | "h" | "cpp" | "sh" | "ini" | "cfg" | "conf" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_list_read() {
        let dir = std::env::temp_dir().join(format!("kron-files-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode("hi");
        handle(&dir, &json!({"op": "write", "path": "sub/a.txt", "data": data})).unwrap();
        let list = handle(&dir, &json!({"op": "list", "path": ""})).unwrap();
        assert_eq!(list["entries"][0]["name"], "sub");
        assert_eq!(list["entries"][0]["dir"], true);
        let read = handle(&dir, &json!({"op": "read", "path": "sub/a.txt"})).unwrap();
        assert_eq!(read["data"], data);
        assert!(handle(&dir, &json!({"op": "read", "path": "sub"})).is_err());
    }
}
