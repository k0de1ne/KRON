//! apply_patch, built from the Codex CLI implementation (openai/codex,
//! Apache-2.0): its parser, fuzzy line matching and line-ending handling are
//! vendored unchanged in the sibling files; this file does the file I/O.

#[rustfmt::skip]
mod parser;
#[rustfmt::skip]
mod seek_sequence;
#[rustfmt::skip]
mod streaming_parser;
#[rustfmt::skip]
mod text_file;

use anyhow::{Context, Result, bail};
use parser::{Hunk, UpdateFileChunk};
use std::path::{Path, PathBuf};

/// Parsed patch, as in codex-apply-patch.
#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub struct ApplyPatchArgs {
    pub hunks: Vec<Hunk>,
    pub patch: String,
    pub workdir: Option<String>,
    pub environment_id: Option<String>,
}

/// From codex-apply-patch file_update.rs: where each chunk lands in the file.
fn compute_replacements(
    original_lines: &[String],
    path: &str,
    chunks: &[UpdateFileChunk],
) -> Result<Vec<text_file::Replacement>> {
    let mut replacements: Vec<text_file::Replacement> = Vec::new();
    let mut line_index: usize = 0;

    for chunk in chunks {
        if let Some(ctx_line) = &chunk.change_context {
            match seek_sequence::seek_sequence(original_lines, std::slice::from_ref(ctx_line), line_index, false) {
                Some(idx) => line_index = idx + 1,
                None => bail!("Failed to find context '{ctx_line}' in {path}"),
            }
        }

        if chunk.old_lines.is_empty() {
            replacements.push((original_lines.len(), 0, chunk.new_lines.clone()));
            continue;
        }

        let mut pattern: &[String] = &chunk.old_lines;
        let mut found = seek_sequence::seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        let mut new_slice: &[String] = &chunk.new_lines;

        // A trailing empty line stands for the file's final newline.
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence::seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        }

        let Some(start_idx) = found else {
            bail!("Failed to find expected lines in {path}:\n{}", chunk.old_lines.join("\n"));
        };
        // Context lines keep their original text and line endings.
        let mut old_start = 0;
        let mut new_start = 0;
        for &(old_context, new_context) in &chunk.context_line_indices {
            if old_context >= pattern.len() || new_context >= new_slice.len() {
                break;
            }
            if old_start != old_context || new_start != new_context {
                replacements.push((
                    start_idx + old_start,
                    old_context - old_start,
                    new_slice[new_start..new_context].to_vec(),
                ));
            }
            old_start = old_context + 1;
            new_start = new_context + 1;
        }
        if old_start != pattern.len() || new_start != new_slice.len() {
            replacements.push((start_idx + old_start, pattern.len() - old_start, new_slice[new_start..].to_vec()));
        }
        line_index = start_idx + pattern.len();
    }

    replacements.sort_by_key(|(index, _, _)| *index);
    Ok(replacements)
}

fn resolve(cwd: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
}

pub fn apply(patch: &str, cwd: &Path) -> Result<String> {
    let args = parser::parse_patch(patch)?;
    if args.hunks.is_empty() {
        bail!("No files were modified.");
    }
    // Work everything out first so a failing hunk leaves no half-applied patch.
    let mut writes: Vec<(PathBuf, Option<String>)> = Vec::new();
    let mut summary = Vec::new();
    for hunk in &args.hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                writes.push((resolve(cwd, path), Some(contents.clone())));
                summary.push(format!("A {}", path.display()));
            }
            Hunk::DeleteFile { path } => {
                let full = resolve(cwd, path);
                if !full.is_file() {
                    bail!("Failed to delete file {}: not found", path.display());
                }
                writes.push((full, None));
                summary.push(format!("D {}", path.display()));
            }
            Hunk::UpdateFile { path, move_path, chunks } => {
                let full = resolve(cwd, path);
                let original = std::fs::read_to_string(&full)
                    .with_context(|| format!("Failed to read file to update {}", path.display()))?;
                let mut source = text_file::SourceFile::parse(&original);
                let replacements = compute_replacements(&source.line_texts(), &path.display().to_string(), chunks)?;
                source.apply_replacements(&replacements);
                let new = source.into_contents();
                match move_path {
                    Some(dest) => {
                        writes.push((full, None));
                        writes.push((resolve(cwd, dest), Some(new)));
                        summary.push(format!("M {}", dest.display()));
                    }
                    None => {
                        writes.push((full, Some(new)));
                        summary.push(format!("M {}", path.display()));
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
        assert!(out.contains("A sub/new.txt") && out.contains("M b.txt"), "{out}");
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
 }
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
    fn keeps_crlf() {
        let d = tmp();
        std::fs::write(d.join("w.txt"), "a\r\nb\r\n").unwrap();
        apply("*** Begin Patch\n*** Update File: w.txt\n a\n-b\n+c\n*** End Patch", &d).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("w.txt")).unwrap(), "a\r\nc\r\n");
    }

    #[test]
    fn heredoc() {
        let cmd = "apply_patch <<'EOF'\n*** Begin Patch\n*** Delete File: x\n*** End Patch\nEOF";
        assert_eq!(from_shell(cmd).unwrap(), "*** Begin Patch\n*** Delete File: x\n*** End Patch");
        assert!(from_shell("ls").is_none());
    }
}
