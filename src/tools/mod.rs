//! Tools the agent can call. The set follows the Codex CLI; scripts in
//! ~/.kron/tools add more (stdin: JSON arguments, stdout: result).

use crate::types::Tool;
use serde_json::{Value, json};
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

/// A short tool set for small local models: Codex's ten tools and long
/// descriptions confuse a 1-3B model, these four it can use.
pub fn small() -> Vec<Tool> {
    let sh = if cfg!(windows) { "PowerShell" } else { "sh" };
    vec![
        tool(
            "shell",
            &format!("Run a {sh} command and get its output."),
            json!({"type": "object", "required": ["command"], "properties": {
                "command": {"type": "string", "description": "The command, e.g. \"ls -la\""}}}),
        ),
        tool(
            "write_file",
            "Create or overwrite a text file.",
            json!({"type": "object", "required": ["path", "content"], "properties": {
                "path": {"type": "string", "description": "File path, e.g. \"notes/hello.txt\""},
                "content": {"type": "string", "description": "The full text to write"}}}),
        ),
        tool(
            "read_file",
            "Read a text file.",
            json!({"type": "object", "required": ["file_path"], "properties": {
                "file_path": {"type": "string", "description": "File path"}}}),
        ),
        tool(
            "list_dir",
            "List files in a folder.",
            json!({"type": "object", "properties": {
                "dir_path": {"type": "string", "description": "Folder path; default is the working folder"}}}),
        ),
    ]
}

/// Tool definitions. Names, parameters and descriptions follow the Codex CLI
/// (openai/codex, Apache-2.0) so models trained on Codex use them well.
pub fn builtin() -> Vec<Tool> {
    let shell_note = if cfg!(windows) { " Commands run in PowerShell." } else { "" };
    vec![
        tool(
            "shell",
            &format!(
                "Runs a shell command and returns its output. Always set the `workdir` param instead of using `cd`.{shell_note}"
            ),
            json!({"type": "object", "required": ["command"], "properties": {
                "command": {"type": "string", "description": "The shell script to execute in the user's default shell."},
                "workdir": {"type": "string", "description": "The working directory to execute the command in."},
                "timeout_ms": {"type": "number", "description": "The timeout for the command in milliseconds (default 120000)."}}}),
        ),
        tool(
            "exec_command",
            &format!("Runs a command, returning output or a session ID for ongoing interaction.{shell_note}"),
            json!({"type": "object", "required": ["cmd"], "properties": {
                "cmd": {"type": "string", "description": "Shell command to execute."},
                "workdir": {"type": "string", "description": "Working directory for the command. Defaults to the turn cwd."},
                "yield_time_ms": {"type": "number", "description": "Wait before yielding output. Defaults to 10000 ms; effective range is 250-30000 ms."}}}),
        ),
        tool(
            "write_stdin",
            "Writes characters to an existing unified exec session and returns recent output.",
            json!({"type": "object", "required": ["session_id"], "properties": {
                "session_id": {"type": "number", "description": "Identifier of the running unified exec session."},
                "chars": {"type": "string", "description": "Bytes to write to stdin. Defaults to empty, which polls without writing."},
                "yield_time_ms": {"type": "number", "description": "Wait before yielding output. Defaults to 2000 ms."}}}),
        ),
        tool(
            "apply_patch",
            APPLY_PATCH_DESCRIPTION,
            json!({"type": "object", "required": ["input"], "properties": {
            "input": {"type": "string", "description": "The entire contents of the apply_patch command"}}}),
        ),
        tool(
            "read_file",
            "Reads a local file with 1-indexed line numbers.",
            json!({"type": "object", "required": ["file_path"], "properties": {
                "file_path": {"type": "string", "description": "Path to the file"},
                "offset": {"type": "number", "description": "The line number to start reading from. Must be 1 or greater."},
                "limit": {"type": "number", "description": "The maximum number of lines to return (default 2000)."}}}),
        ),
        tool(
            "list_dir",
            "Lists entries in a local directory with 1-indexed entry numbers and simple type labels.",
            json!({"type": "object", "required": ["dir_path"], "properties": {
                "dir_path": {"type": "string", "description": "Path to the directory to list."},
                "offset": {"type": "number", "description": "The entry number to start listing from. Must be 1 or greater."},
                "limit": {"type": "number", "description": "The maximum number of entries to return."},
                "depth": {"type": "number", "description": "The maximum directory depth to traverse. Must be 1 or greater."}}}),
        ),
        tool(
            "grep_files",
            "Finds lines matching a regular expression (respects .gitignore). Returns path:line: text.",
            json!({"type": "object", "required": ["pattern"], "properties": {
                "pattern": {"type": "string", "description": "Regular expression pattern to search for."},
                "include": {"type": "string", "description": "Optional glob that limits which files are searched (e.g. \"*.rs\" or \"*.{ts,tsx}\")."},
                "path": {"type": "string", "description": "Directory or file path to search. Defaults to the session's working directory."},
                "limit": {"type": "number", "description": "Maximum number of matches to return (defaults to 200)."}}}),
        ),
        tool(
            "update_plan",
            "Updates the task plan.\nProvide an optional explanation and a list of plan items, each with a step and status.\nAt most one step can be in_progress at a time.\n",
            json!({"type": "object", "required": ["plan"], "properties": {
                "explanation": {"type": "string", "description": "Optional explanation for this plan update."},
                "plan": {"type": "array", "description": "The list of steps", "items": {"type": "object", "required": ["step", "status"], "properties": {
                    "step": {"type": "string", "description": "Task step text."},
                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed"], "description": "Step status."}}}}}}),
        ),
        tool(
            "web_search",
            "Searches the web with the model provider's native search and returns a sourced answer.",
            json!({"type": "object", "required": ["query"], "properties": {"query": {"type": "string"}}}),
        ),
        tool(
            "spawn_agent",
            "Hands a self-contained task to another agent (see the agent list in your instructions) and returns its \
            final answer. Several calls run in parallel.",
            json!({"type": "object", "required": ["task"], "properties": {
                "agent": {"type": "string", "description": "Agent name (default: same as you)"},
                "task": {"type": "string"}}}),
        ),
    ]
}

/// Instructions for the JSON (non-freeform) apply_patch tool, modelled on Codex's.
const APPLY_PATCH_DESCRIPTION: &str = r#"Use the `apply_patch` tool to edit files.
Your patch language is a stripped-down, file-oriented diff format designed to be easy to parse and safe to apply. You can think of it as a high-level envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Within that envelope, you get a sequence of file operations.
You MUST include a header to specify the action you are taking.
Each operation starts with one of three headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. Nothing follows.
*** Update File: <path> - patch an existing file in place (optionally with a rename).

May be immediately followed by *** Move to: <new path> if you want to rename the file.
Then one or more "hunks", each introduced by @@ (optionally followed by a hunk header).
Within a hunk each line starts with:

For instructions on [context_before] and [context_after]:
- By default, show 3 lines of code immediately above and 3 lines immediately below each change. If a change is within 3 lines of a previous change, do NOT duplicate the first change's [context_after] lines in the second change's [context_before] lines.
- If 3 lines of context is insufficient to uniquely identify the snippet of code within the file, use the @@ operator to indicate the class or function to which the snippet belongs.
- If a code block is repeated so many times in a class or function such that even a single `@@` statement and 3 lines of context cannot uniquely identify the snippet of code, you can use multiple `@@` statements to jump to the right context.

The full grammar definition is below:
Patch := Begin { FileOp } End
Begin := "*** Begin Patch" NEWLINE
End := "*** End Patch" NEWLINE
FileOp := AddFile | DeleteFile | UpdateFile
AddFile := "*** Add File: " path NEWLINE { "+" line NEWLINE }
DeleteFile := "*** Delete File: " path NEWLINE
UpdateFile := "*** Update File: " path NEWLINE [ MoveTo ] { Hunk }
MoveTo := "*** Move to: " newPath NEWLINE
Hunk := "@@" [ header ] NEWLINE { HunkLine } [ "*** End of File" NEWLINE ]
HunkLine := (" " | "-" | "+") text NEWLINE

A full patch can combine several operations:

*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch

It is important to remember:

- You must include a header with your intended action (Add/Delete/Update)
- You must prefix new lines with `+` even when creating a new file
- File references can only be relative, NEVER ABSOLUTE.
"#;

/// Lark grammar of the freeform apply_patch tool (for OpenAI Responses models).
pub const APPLY_PATCH_LARK: &str = include_str!("../prompts/apply_patch.lark");

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
            Some(p) if Path::new(p).is_absolute() => match crate::sandbox::get() {
                Some(sb) => sb.host_path(Path::new(p)),
                None => PathBuf::from(p),
            },
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
            // Small models sometimes send the argv form.
            let joined =
                a["command"].as_array().map(|v| v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "));
            let command = match &joined {
                Some(c) => c.as_str(),
                None => need("command")?,
            };
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
        "write_file" => {
            let file = path("path");
            need("path")?;
            let content = s("content").unwrap_or("");
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&file, content)?;
            Ok(format!("Wrote {} bytes to {}", content.len(), file.display()))
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
