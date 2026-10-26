//! Codex's apply_patch format:
//!
//! *** Begin Patch
//! *** Add File: path        (+lines)
//! *** Delete File: path
//! *** Update File: path
//! *** Move to: new/path     (optional)
//! @@ optional context line
//!  context / -removed / +added
//! *** End of File           (optional, anchors the hunk to the end)
//! *** End Patch

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
enum Op {
    Add { path: String, content: String },
    Delete { path: String },
    Update { path: String, move_to: Option<String>, chunks: Vec<Chunk> },
}

#[derive(Debug, PartialEq, Default)]
struct Chunk {
    context: Option<String>,
    old: Vec<String>,
    new: Vec<String>,
    eof: bool,
}

fn parse(patch: &str) -> Result<Vec<Op>> {
    let lines: Vec<&str> = patch.trim().lines().collect();
    let start =
        lines.iter().position(|l| l.trim() == "*** Begin Patch").context("patch must start with '*** Begin Patch'")?;
    let end = lines.iter().rposition(|l| l.trim() == "*** End Patch").context("patch must end with '*** End Patch'")?;
    let lines = &lines[start + 1..end];

    let mut ops = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            i += 1;
            let mut content = String::new();
            while i < lines.len() && !lines[i].starts_with("*** ") {
                let l = lines[i]
                    .strip_prefix('+')
                    .with_context(|| format!("Add File lines must start with '+': {}", lines[i]))?;
                content.push_str(l);
                content.push('\n');
                i += 1;
            }
            ops.push(Op::Add { path: path.trim().into(), content });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            ops.push(Op::Delete { path: path.trim().into() });
            i += 1;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            i += 1;
            let mut move_to = None;
            if let Some(m) = lines.get(i).and_then(|l| l.strip_prefix("*** Move to: ")) {
                move_to = Some(m.trim().to_string());
                i += 1;
            }
            let mut chunks: Vec<Chunk> = Vec::new();
            while i < lines.len() && (!lines[i].starts_with("*** ") || lines[i] == "*** End of File") {
                let l = lines[i];
                if l == "*** End of File" {
                    if let Some(c) = chunks.last_mut() {
                        c.eof = true;
                    }
                } else if let Some(ctx) = l.strip_prefix("@@") {
                    let ctx = ctx.trim();
                    chunks.push(Chunk { context: (!ctx.is_empty()).then(|| ctx.to_string()), ..Default::default() });
                } else {
                    if chunks.is_empty() {
                        chunks.push(Chunk::default());
                    }
                    let c = chunks.last_mut().unwrap();
                    match l.chars().next() {
                        Some('+') => c.new.push(l[1..].to_string()),
                        Some('-') => c.old.push(l[1..].to_string()),
                        Some(' ') => {
                            c.old.push(l[1..].to_string());
                            c.new.push(l[1..].to_string());
                        }
                        // Models often drop the space on empty context lines.
                        None => {
                            c.old.push(String::new());
                            c.new.push(String::new());
                        }
                        _ => bail!("invalid patch line in {path}: '{l}'"),
                    }
                }
                i += 1;
            }
            ops.push(Op::Update { path: path.trim().into(), move_to, chunks });
        } else if line.trim().is_empty() {
            i += 1;
        } else {
            bail!("unexpected patch line: '{line}'");
        }
    }
    if ops.is_empty() {
        bail!("patch contains no operations");
    }
    Ok(ops)
}

/// Find `pat` in `lines` at or after `from`, tolerating whitespace differences.
fn seek(lines: &[String], pat: &[String], from: usize, eof: bool) -> Option<usize> {
    if pat.is_empty() {
        return Some(lines.len());
    }
    if pat.len() > lines.len() {
        return None;
    }
    let last = lines.len() - pat.len();
    let starts: Vec<usize> =
        if eof { std::iter::once(last).chain(from..=last).collect() } else { (from..=last).collect() };
    let norms: [fn(&str) -> String; 3] = [|s| s.to_string(), |s| s.trim_end().to_string(), |s| s.trim().to_string()];
    for norm in norms {
        for &s in &starts {
            if s < from {
                continue;
            }
            if pat.iter().enumerate().all(|(k, p)| norm(&lines[s + k]) == norm(p)) {
                return Some(s);
            }
        }
    }
    None
}

fn apply_chunks(text: &str, chunks: &[Chunk], path: &str) -> Result<String> {
    let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut edits: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut cursor = 0;
    for c in chunks {
        if let Some(ctx) = &c.context {
            let at = seek(&lines, std::slice::from_ref(ctx), cursor, false)
                .with_context(|| format!("{path}: context '@@ {ctx}' not found"))?;
            cursor = at + 1;
        }
        if c.old.is_empty() {
            edits.push((lines.len(), 0, c.new.clone()));
            continue;
        }
        let mut old = c.old.clone();
        let mut new = c.new.clone();
        let mut at = seek(&lines, &old, cursor, c.eof);
        if at.is_none() && old.last().is_some_and(|l| l.is_empty()) {
            old.pop();
            if new.last().is_some_and(|l| l.is_empty()) {
                new.pop();
            }
            at = seek(&lines, &old, cursor, c.eof);
        }
        let at = at.with_context(|| format!("{path}: could not find lines to replace:\n{}", c.old.join("\n")))?;
        edits.push((at, old.len(), new));
        cursor = at + old.len();
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    for (at, len, new) in edits {
        lines.splice(at..at + len, new);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

pub fn apply(patch: &str, cwd: &Path) -> Result<String> {
    let ops = parse(patch)?;
    // Compute everything first so a failing hunk leaves no half-applied patch.
    let mut writes: Vec<(PathBuf, Option<String>)> = Vec::new();
    let mut summary = Vec::new();
    for op in &ops {
        match op {
            Op::Add { path, content } => {
                writes.push((resolve(cwd, path), Some(content.clone())));
                summary.push(format!("A {path}"));
            }
            Op::Delete { path } => {
                let full = resolve(cwd, path);
                if !full.exists() {
                    bail!("{path}: file to delete does not exist");
                }
                writes.push((full, None));
                summary.push(format!("D {path}"));
            }
            Op::Update { path, move_to, chunks } => {
                let full = resolve(cwd, path);
                let text = std::fs::read_to_string(&full).with_context(|| format!("{path}: cannot read"))?;
                let new = apply_chunks(&text, chunks, path)?;
                match move_to {
                    Some(dest) => {
                        writes.push((full, None));
                        writes.push((resolve(cwd, dest), Some(new)));
                        summary.push(format!("M {path} -> {dest}"));
                    }
                    None => {
                        writes.push((full, Some(new)));
                        summary.push(format!("M {path}"));
                    }
                }
            }
        }
    }
    for (path, content) in writes {
        match content {
            Some(c) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, c)?;
            }
            None => std::fs::remove_file(&path)?,
        }
    }
    Ok(format!("Success. Updated the following files:\n{}", summary.join("\n")))
}

/// Pull a patch out of a shell command like `apply_patch <<'EOF' ... EOF`.
pub fn from_shell(command: &str) -> Option<String> {
    let c = command.trim_start();
    if !(c.starts_with("apply_patch") || c.starts_with("applypatch")) || !c.contains("*** Begin Patch") {
        return None;
    }
    let start = c.find("*** Begin Patch")?;
    let end = c.rfind("*** End Patch")? + "*** End Patch".len();
    Some(c[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("kron-patch-{}", crate::types::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn add_update_delete_move() {
        let d = tmp();
        std::fs::write(d.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        std::fs::write(d.join("gone.txt"), "x\n").unwrap();
        let patch = "*** Begin Patch
*** Add File: sub/new.txt
+hello
+world
*** Delete File: gone.txt
*** Update File: a.txt
*** Move to: b.txt
@@ one
 two
-three
+THREE
 four
*** End Patch";
        let out = apply(patch, &d).unwrap();
        assert!(out.contains("A sub/new.txt"));
        assert_eq!(std::fs::read_to_string(d.join("sub/new.txt")).unwrap(), "hello\nworld\n");
        assert!(!d.join("gone.txt").exists());
        assert!(!d.join("a.txt").exists());
        assert_eq!(std::fs::read_to_string(d.join("b.txt")).unwrap(), "one\ntwo\nTHREE\nfour\n");
    }

    #[test]
    fn fuzzy_whitespace_and_eof() {
        let d = tmp();
        std::fs::write(d.join("f.rs"), "fn a() {\n    x();   \n}\nfn b() {\n    x();\n}\n").unwrap();
        let patch = "*** Begin Patch
*** Update File: f.rs
@@ fn b() {
-    x();
+    y();
*** End of File
*** End Patch";
        apply(patch, &d).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("f.rs")).unwrap(),
            "fn a() {\n    x();   \n}\nfn b() {\n    y();\n}\n"
        );
    }

    #[test]
    fn failing_hunk_changes_nothing() {
        let d = tmp();
        std::fs::write(d.join("f.txt"), "a\n").unwrap();
        let patch = "*** Begin Patch\n*** Add File: n.txt\n+x\n*** Update File: f.txt\n-zzz\n+y\n*** End Patch";
        assert!(apply(patch, &d).is_err());
        assert!(!d.join("n.txt").exists());
    }

    #[test]
    fn heredoc() {
        let cmd = "apply_patch <<'EOF'\n*** Begin Patch\n*** Delete File: x\n*** End Patch\nEOF";
        assert_eq!(from_shell(cmd).unwrap(), "*** Begin Patch\n*** Delete File: x\n*** End Patch");
        assert!(from_shell("ls").is_none());
    }
}
