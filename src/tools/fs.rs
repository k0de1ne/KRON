//! read_file, list_dir, grep_files — modelled on the Codex CLI tools.

use anyhow::{Context, Result, bail};
use std::path::Path;

pub fn read_file(path: &Path, offset: usize, limit: usize) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let total = text.lines().count();
    let offset = offset.max(1);
    if offset > total.max(1) {
        bail!("offset {offset} is past the end of the file ({total} lines)");
    }
    let mut out = String::new();
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let line: String = if line.chars().count() > 500 {
            line.chars().take(500).collect::<String>() + "…"
        } else {
            line.to_string()
        };
        out.push_str(&format!("L{}: {}\n", i + 1, line));
    }
    if offset - 1 + limit < total {
        out.push_str(&format!("… {} more lines (use offset to continue)\n", total - (offset - 1 + limit)));
    }
    Ok(out)
}

pub fn list_dir(dir: &Path, depth: usize, offset: usize, limit: usize) -> Result<String> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let mut entries = Vec::new();
    walk(dir, 0, depth, &mut entries);
    let total = entries.len();
    let offset = offset.max(1);
    let mut out = format!("Absolute path: {}\n", dir.display());
    for e in entries.into_iter().skip(offset - 1).take(limit) {
        out.push_str(&e);
        out.push('\n');
    }
    if offset - 1 + limit < total {
        out.push_str(&format!("… {} more entries (use offset to continue)\n", total - (offset - 1 + limit)));
    }
    Ok(out)
}

fn walk(dir: &Path, level: usize, depth: usize, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut items: Vec<_> = rd.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" {
            continue;
        }
        let ft = e.file_type().ok();
        let is_dir = ft.as_ref().is_some_and(|t| t.is_dir());
        let suffix = if is_dir {
            "/"
        } else if ft.as_ref().is_some_and(|t| t.is_symlink()) {
            "@"
        } else {
            ""
        };
        out.push(format!("{}{}{}", "  ".repeat(level), name, suffix));
        if is_dir && level + 1 < depth {
            walk(&e.path(), level + 1, depth, out);
        }
    }
}

/// Regex search over files (respecting .gitignore). Returns `path:line: text`.
pub fn grep(root: &Path, pattern: &str, include: Option<&str>, limit: usize) -> Result<String> {
    let re = regex::Regex::new(pattern).with_context(|| format!("invalid regex '{pattern}'"))?;
    let mut builder = ignore::WalkBuilder::new(root);
    builder.hidden(false).filter_entry(|e| e.file_name() != ".git");
    if let Some(glob) = include {
        let mut ob = ignore::overrides::OverrideBuilder::new(root);
        for g in glob.split(',') {
            ob.add(g.trim())?;
        }
        builder.overrides(ob.build()?);
    }
    let mut out = Vec::new();
    'files: for entry in builder.build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let shown = entry.path().strip_prefix(root).unwrap_or(entry.path()).display().to_string();
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                let line: String = line.trim().chars().take(300).collect();
                out.push(format!("{shown}:{}: {line}", i + 1));
                if out.len() >= limit {
                    out.push(format!("… stopped at {limit} matches"));
                    break 'files;
                }
            }
        }
    }
    Ok(if out.is_empty() { "No matches found.".into() } else { out.join("\n") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_list_grep() {
        let d = std::env::temp_dir().join(format!("kron-fs-{}", crate::types::new_id()));
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/a.rs"), "fn main() {}\n// TODO one\n").unwrap();
        std::fs::write(d.join("b.txt"), "TODO two\n").unwrap();

        assert_eq!(read_file(&d.join("src/a.rs"), 2, 10).unwrap(), "L2: // TODO one\n");
        let ls = list_dir(&d, 2, 1, 100).unwrap();
        assert!(ls.contains("src/\n  a.rs"));
        let g = grep(&d, "TODO", Some("*.rs"), 10).unwrap();
        assert!(g.contains("a.rs:2: // TODO one") && !g.contains("b.txt"));
    }
}
