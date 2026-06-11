# PLEXUS

Minimal autonomous agent engine written in Rust.

## How it works

PLEXUS gives an LLM a task and a set of tools, then runs a loop until the task is done:

```
task → [LLM] → tool calls → [execute] → results → [LLM] → ... → answer
```

Streaming output, parallel tool execution, pre/post hooks.

## Usage

```bash
# Run a task
plexus "list all .rs files and count lines of code"

# Read task from stdin
echo "what time is it?" | plexus

# Watch a directory for .task files (file-based trigger)
plexus --watch ./inbox/

# HTTP server with web UI (built with --features http)
plexus --serve --port 8080
```

Drop a `.task` file in `inbox/` → agent runs → `.result` appears next to it.

## Configuration

`config.toml` (created automatically with defaults if missing):

```toml
[llm]
provider  = "openai"          # openai | anthropic | external
model     = "gpt-4o"
# api_key = "sk-..."          # or set OPENAI_API_KEY / ANTHROPIC_API_KEY

[agent]
tool_paths    = ["./extensions/tools"]
hooks_dir     = "./extensions/hooks"
context_file  = "./system.md"
```

### External LLM provider

Any process that speaks JSON on stdin/stdout can be an LLM backend:

```toml
[llm]
provider = "external"
command  = "./extensions/llm/my_model.py"
```

stdin → `{"system": "...", "messages": [...], "tools": [...]}` \
stdout ← `{"text": "...", "tool_calls": [...], "done": true}`

## Extension points

| Directory | What goes there |
|---|---|
| `tools/` | External tool scripts (override or extend built-in tools) |
| `hooks/on_start/` | Scripts run before each task (stdin: `{"task": "..."}`) |
| `hooks/on_done/` | Scripts run after each task (stdin: `{"task", "output", "tool_calls"}`) |
| `hooks/` | Context hook overrides (`{{cwd}}`, `{{git_status}}`, custom `{{my_hook}}`) |
| `inbox/` | Drop `.task` files here when using `--watch` |
| `llm/` | Suggested location for external LLM scripts |
| `sessions/` | Reserved for session history |

### Built-in tools

`shell`, `read_file`, `write_file`, `edit_file`, `grep`, `list_files`

Any executable in `tools/` with a matching name overrides a built-in.

### Built-in context hooks

`{{cwd}}`, `{{git_status}}`, `{{time}}`, `{{env.VAR}}`, `{{file.PATH}}`

Any script in `hooks/` with a matching name overrides a built-in.

## Build variants

Feature flags let you compile exactly what you need:

```bash
cargo build --no-default-features             # Q&A only (single LLM call)
cargo build --no-default-features -F hooks    # Q&A + pre/post hooks
cargo build --no-default-features -F loop     # agent loop, no tools
cargo build --no-default-features -F trigger  # loop + file watcher
cargo build                                   # default: loop+tools+hooks+context
cargo build -F full                           # everything including HTTP server
```

| Feature | Adds |
|---|---|
| `loop` | agent loop (multi-turn, tool calling) |
| `tools` | built-in tools + external tool scripts |
| `hooks` | `on_start` / `on_done` lifecycle hooks |
| `context` | `{{variable}}` expansion in system prompt |
| `trigger` | `--watch` file-based trigger |
| `http` | HTTP server with SSE streaming and web UI |
| `full` | all of the above |

## Architecture

```
src/
  proto/      types and traits (Config, Tool, Message, LlmBackend, EventSink)
  context/    {{variable}} expansion in system prompts
  tools/      built-in tools + external script discovery
  llm/        LLM backends: OpenAI, Anthropic, external process
  hooks/      pre/post lifecycle hook runner
  trigger/    file-based trigger (--watch)
  agent/      the agent loop
  clients/    CLI binary + HTTP server
```

Each module is an optional Cargo feature. The crates are also independently usable — embed `agent` in your own binary, or use `llm` as a standalone LLM client library.
