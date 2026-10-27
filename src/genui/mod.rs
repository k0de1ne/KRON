//! Generative UI: instead of answering in a chat, the agent builds a working
//! interface for the task and keeps it up to date.
//!
//! - The agent works as usual (same tools, same permissions, same stop button)
//!   and gets three more tools: `render_ui`, `update_data`, `remember`.
//! - A surface (see `surface`) is declarative: components from a fixed catalog,
//!   rendered by the client with one visual language. Where the catalog is not
//!   enough, an `Html` component runs the agent's own code in a sandboxed frame.
//! - What the person likes and uses is learned (see `profile`); layouts that
//!   worked are reused for similar tasks, and a surface is changed only as much
//!   as the request needs.
//! - The client streams the run's events (text, steps, approvals) plus
//!   `surface` events with the new state, like AG-UI's state snapshots.

pub mod profile;
pub mod surface;

use crate::agent::{Access, Agent, Executor, Extra};
use crate::config::Config;
use crate::store::Store;
use crate::turn::TurnReq;
use crate::types::{Event, Message, Sink, Tool};
use anyhow::Result;
use profile::Memory;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use surface::{Surface, Surfaces};

pub fn surfaces(cfg: &Config) -> Surfaces {
    Surfaces::new(cfg.path("genui").join("surfaces"))
}

pub fn memory(cfg: &Config) -> Memory {
    Memory::new(cfg.path("genui"))
}

/// The surface a thread works on, if any.
pub fn thread_surface(store: &Store, thread: &str) -> Option<String> {
    store.meta(thread)["surface"].as_str().map(String::from)
}

/// Interfaces need a model that handles tools and long instructions; small
/// local models answer in text instead.
pub fn capable(cfg: &Config, req: &TurnReq) -> bool {
    let agent = req.agent.as_deref().and_then(|a| cfg.agents.get(a));
    let name =
        req.provider.clone().or_else(|| agent.and_then(|a| a.provider.clone())).or_else(|| cfg.default_provider().ok());
    name.and_then(|n| cfg.provider(&n).ok().map(|p| p.kind != "local")).unwrap_or(false)
}

/// One turn in "ui" mode.
pub async fn turn(
    cfg: &Arc<Config>,
    store: &Store,
    req: &TurnReq,
    exec: Arc<dyn Executor>,
    sink: &dyn Sink,
    history: Vec<Message>,
) -> Result<()> {
    let surfaces = surfaces(cfg);
    let memory = memory(cfg);
    let current = thread_surface(store, &req.thread).and_then(|id| surfaces.get(&id).ok());
    let action = parse_action(&req.text);
    let request = match &action {
        Some(a) => a["name"].as_str().unwrap_or("").to_string(),
        None => req.text.clone(),
    };
    let (guess, _) = profile::classify(&request);
    let intent = current.as_ref().map(|s| s.intent.clone()).filter(|i| !i.is_empty()).unwrap_or(guess);

    // A layout that worked for this kind of task: shown at once while the
    // agent gathers the real data, and given to the agent to build from.
    let template = if current.is_none() && intent != "chat" { memory.best_template(&intent, &request) } else { None };
    if let Some((t, _)) = &template {
        memory.used_template(&t.id);
        sink.emit(Event::Surface {
            surface: json!({"id": "", "title": t.title, "intent": t.intent, "root": t.root,
                "components": t.components, "data": t.data, "template": t.id}),
            draft: true,
        });
    }

    let tools = Arc::new(UiTools {
        surfaces: surfaces.clone(),
        memory: memory.clone(),
        store: store.clone(),
        thread: req.thread.clone(),
        request: request.clone(),
        intent: intent.clone(),
        template: template.as_ref().map(|(t, _)| t.id.clone()),
        current: Mutex::new(current.as_ref().map(|s| s.id.clone())),
        system: system_prompt(&memory, &intent, current.as_ref(), template.as_ref().map(|(t, _)| t)),
        failures: Mutex::new(0),
    });
    let mut agent = Agent::new(
        cfg,
        req.agent.as_deref(),
        req.provider.as_deref(),
        req.model.as_deref(),
        req.effort.as_deref(),
        exec,
    )?;
    agent.access = Access::parse(&req.access);
    agent.extra = Some(tools.clone());
    let thread = req.thread.clone();
    agent
        .run(history, sink, &|m| {
            if let Err(e) = store.append(&thread, m) {
                eprintln!("cannot save message: {e}");
            }
        })
        .await?;
    Ok(())
}

/// A person's click on the surface reaches the agent as a message
/// `[ui-action] {"name": ..., "context": {...}, "component": ...}`.
pub fn parse_action(text: &str) -> Option<Value> {
    let rest = text.trim().strip_prefix("[ui-action]")?;
    serde_json::from_str(rest.trim()).ok()
}

struct UiTools {
    surfaces: Surfaces,
    memory: Memory,
    store: Store,
    thread: String,
    request: String,
    intent: String,
    template: Option<String>,
    current: Mutex<Option<String>>,
    system: String,
    /// Invalid render_ui calls in this turn: after three, fall back to text.
    failures: Mutex<u32>,
}

#[async_trait::async_trait]
impl Extra for UiTools {
    fn tools(&self) -> Vec<Tool> {
        ui_tools()
    }

    fn system(&self) -> String {
        self.system.clone()
    }

    async fn call(&self, name: &str, args: &Value, sink: &dyn Sink) -> Option<String> {
        let out = match name {
            "render_ui" => self.render(args, sink),
            "update_data" => self.update(args, sink),
            "remember" => match args["note"].as_str().filter(|n| !n.trim().is_empty()) {
                Some(n) => {
                    self.memory.remember(n).map(|_| "Saved. It will be applied to future interfaces.".to_string())
                }
                None => Err(anyhow::anyhow!("missing 'note'")),
            },
            _ => return None,
        };
        Some(out.unwrap_or_else(|e| format!("error: {e:#}")))
    }
}

impl UiTools {
    fn render(&self, args: &Value, sink: &dyn Sink) -> Result<String> {
        let mut spec = object_args(args)?;
        // Some models send the surface as a JSON string.
        if let Some(text) = args["surface"].as_str().or(args["components"].as_str()) {
            let parsed: Value =
                serde_json::from_str(text).map_err(|e| anyhow::anyhow!("components is not valid JSON: {e}"))?;
            spec = if parsed.is_array() {
                json!({"components": parsed, "title": args["title"], "data": args["data"]})
            } else {
                parsed
            };
        }
        if !spec.is_object() {
            anyhow::bail!("render_ui takes an object {{title, components, data}}");
        }
        if let Some(d) = spec["data"].as_str() {
            spec["data"] = serde_json::from_str(d).map_err(|e| anyhow::anyhow!("data is not valid JSON: {e}"))?;
        }
        if spec["intent"].as_str().is_none_or(str::is_empty) {
            spec["intent"] = json!(self.intent);
        }
        let current = self.current.lock().unwrap().clone();
        let result = self.surfaces.render(current.as_deref(), &self.thread, &spec, "agent");
        let s = match result {
            Ok(s) => s,
            Err(e) => {
                let mut f = self.failures.lock().unwrap();
                *f += 1;
                if *f >= 3 {
                    return Ok(format!(
                        "error: {e:#}\n\nThe interface could not be built after several tries. Do not call render_ui \
                        again in this turn: answer the user in plain text (markdown) instead."
                    ));
                }
                anyhow::bail!("{e:#}\nFix these problems and call render_ui again with the whole surface.");
            }
        };
        let first = current.is_none();
        if first {
            *self.current.lock().unwrap() = Some(s.id.clone());
            self.store.set_meta(&self.thread, "surface", json!(s.id))?;
            let s = self.surfaces.update(&s.id, |x| {
                x.request = self.request.clone();
                x.template = self.template.clone();
                if x.title.is_empty() {
                    x.title = crate::types::preview(&self.request, 60);
                }
            })?;
            self.store.set_meta(&self.thread, "title", json!(s.title)).ok();
            self.finish(&s, true, sink);
        } else {
            self.finish(&s, false, sink);
        }
        Ok(format!(
            "Rendered '{}' (version {}, {} components). The user sees it now. Reply with at most one short sentence; \
            do not repeat what the interface shows.",
            s.title,
            s.version,
            s.components.len()
        ))
    }

    fn update(&self, args: &Value, sink: &dyn Sink) -> Result<String> {
        let args = &object_args(args)?;
        let Some(id) = self.current.lock().unwrap().clone() else {
            anyhow::bail!("there is no interface yet: call render_ui first");
        };
        let patches: Vec<(String, Option<Value>)> = args["patches"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("'patches' must be an array of {{path, value}}"))?
            .iter()
            .map(|p| (p["path"].as_str().unwrap_or("").to_string(), p.get("value").filter(|v| !v.is_null()).cloned()))
            .collect();
        if patches.is_empty() || patches.iter().any(|(p, _)| p.is_empty()) {
            anyhow::bail!("every patch needs a 'path' (JSON Pointer such as /rows/0/status)");
        }
        let s = self.surfaces.patch_data(&id, &patches, "agent")?;
        self.finish(&s, false, sink);
        Ok(format!("Updated {} value(s); version {}.", patches.len(), s.version))
    }

    fn finish(&self, s: &Surface, first: bool, sink: &dyn Sink) {
        if let Err(e) = self.memory.rendered(s, first) {
            eprintln!("cannot update the profile: {e:#}");
        }
        sink.emit(Event::Surface { surface: s.to_client(), draft: false });
    }
}

/// Tool arguments as an object. Providers hand over arguments that are not
/// valid JSON as a string; a call cut short (unclosed brackets) is closed here,
/// anything else goes back to the model to fix.
fn object_args(args: &Value) -> Result<Value> {
    let Some(text) = args.as_str() else { return Ok(args.clone()) };
    let parsed = serde_json::from_str::<Value>(text)
        .or_else(|e| serde_json::from_str::<Value>(&close_json(text)).map_err(|_| e));
    match parsed {
        Ok(v) if v.is_object() => Ok(v),
        Ok(_) => anyhow::bail!("arguments must be a JSON object"),
        Err(e) => anyhow::bail!("the arguments are not valid JSON ({e}); call the tool again with valid JSON"),
    }
}

/// Close the strings, arrays and objects left open at the end of `text`.
fn close_json(text: &str) -> String {
    let (mut stack, mut in_str, mut esc) = (Vec::new(), false, false);
    for c in text.chars() {
        match (in_str, c) {
            (true, _) if esc => esc = false,
            (true, '\\') => esc = true,
            (true, '"') => in_str = false,
            (true, _) => {}
            (false, '"') => in_str = true,
            (false, '{') => stack.push('}'),
            (false, '[') => stack.push(']'),
            (false, '}' | ']') => {
                stack.pop();
            }
            _ => {}
        }
    }
    let mut out = text.trim_end().trim_end_matches(',').to_string();
    if in_str {
        out.push('"');
    }
    out.extend(stack.iter().rev());
    out
}

fn ui_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "render_ui".into(),
            description: "Show the user an interactive interface for the task (creates it, or replaces the structure \
                of the current one). Pass the WHOLE surface: every component, flat, children referenced by id. \
                `data` is merged into the current data model (top-level keys) unless replace_data is true."
                .into(),
            parameters: json!({"type": "object", "required": ["title", "components"], "properties": {
                "title": {"type": "string", "description": "Short name of the interface, in the user's language"},
                "intent": {"type": "string", "description": "Kind of task: compare, project, server, code, data, plan, write or your own word"},
                "components": {"type": "array", "description": "Flat list of components; one has id \"root\"", "items": {"type": "object"}},
                "data": {"type": "object", "description": "Data model the components bind to with {\"path\": \"/...\"}"},
                "replace_data": {"type": "boolean"},
                "note": {"type": "string", "description": "What changed, for the version history"}}}),
        },
        Tool {
            name: "update_data".into(),
            description: "Change values in the current interface's data model without rebuilding it (new results, \
                statuses, rows). Faster than render_ui and keeps everything the user set up."
                .into(),
            parameters: json!({"type": "object", "required": ["patches"], "properties": {
                "patches": {"type": "array", "items": {"type": "object", "required": ["path"], "properties": {
                    "path": {"type": "string", "description": "JSON Pointer, e.g. /rows/2/status or /logs/- to append"},
                    "value": {"description": "New value; omit or null to remove"}}}}}}),
        },
        Tool {
            name: "remember".into(),
            description: "Save a lasting preference of the user about how they like to work or see things \
                (e.g. 'prefers tables over charts', 'show prices in EUR'). Only for things they would want applied every time."
                .into(),
            parameters: json!({"type": "object", "required": ["note"], "properties": {
                "note": {"type": "string"}}}),
        },
    ]
}

/// Component catalog as the model sees it.
pub const CATALOG: &str = r#"Every component: {"id": "unique", "component": "<Type>", ...props}. Props marked (b) accept a literal or a binding {"path": "/json/pointer"} into `data`.
Layout:
- Column {children: [ids], gap?: "s"|"m"|"l"} — vertical stack. Make the root a Column.
- Row {children: [ids], align?: "start"|"center"|"end"|"between"} — horizontal, wraps on phones.
- Grid {children: [ids], min?: 220} — responsive grid; columns at least `min` px wide (1 column on phones).
- Card {title?, subtitle?, children: [ids]}
- Tabs {tabs: [{label, children: [ids]}]}
- Collapsible {title, open?: bool, children: [ids]}
- Divider {}
Children may also be a template: {"path": "/items", "componentId": "item_card"} renders item_card once per array element; inside it, paths without a leading "/" are relative to the element (e.g. {"path": "name"}).
Content:
- Heading {text(b), level?: 1|2|3}
- Text {text(b) markdown, tone?: "muted"}
- Badge {text(b), tone?: "ok"|"warn"|"danger"|"info"|"neutral"}
- Metric {label, value(b), unit?, delta?(b) e.g. "+12%", tone?, hint?}
- Progress {label?, value(b) 0..100, tone?}
- KeyValue {items(b): [{key, value}]}
- Code {code(b), lang?}
- Diff {patch(b) unified diff} or {before(b), after(b), path?}
- Log {lines(b): string or [strings], title?, height?: px} — command output, logs; auto-scrolls.
- Image {src: "data:" or "https:" URL, alt}
Data (bind rows to arrays in `data`):
- Table {rows(b): [{...}], columns: [{key, label, type?: "text"|"number"|"badge"|"progress"|"link"|"code", editable?: bool}], rowKey?: "id", search?: bool, selectable?: "/path" (writes selected row keys there), filter?: {key, path} (show rows whose `key` is in the array/value at `path`), rowAction?: action, pageSize?: 50}. Sorting by any column is built in.
- Chart {kind: "bar"|"hbar"|"line"|"area"|"donut", rows(b): [{...}], x: "key for labels", series: [{key, label?}], filter?: {key, path}, height?: px, unit?}
- List {items(b): [{id, title, subtitle?, badge?, tone?, done?}], checkable?: bool, reorderable?: bool, removable?: bool, itemAction?: action} — edits write back to the bound array.
- Tree {nodes(b): [{name, children?: [...], badge?, hint?}], itemAction?: action} — file trees, hierarchies. The clicked node is in the action context as `node`.
Inputs (two-way bound: `value` must be a binding):
- TextField {label?, value(b), placeholder?, multiline?: bool, type?: "text"|"number"|"password"}
- Select {label?, value(b), options: ["a", "b"] or [{value, label}]}
- ChoicePicker {label?, value(b), options, multiple?: bool} — chips; great for filters.
- CheckBox {label, value(b)}
- Slider {label?, value(b), min, max, step?, unit?}
- Editor {value(b), lang?, label?, height?: px} — code/text editor.
- Button {label, action, variant?: "primary"|"default"|"danger"|"ghost", icon?: "play"|"refresh"|"trash"|"save"|"plus"|"check"|"download"|"terminal"}
- Terminal {log(b) "/path to an array of lines", cwd?, placeholder?} — the user types commands; they run on the machine (with the user's permission rules) and the output is appended to `log`.
Escape hatch:
- Html {html: "full self-contained HTML (inline CSS/JS)", height?: px} — ONLY when the catalog cannot express it (a custom visualization, a game, a canvas tool). Runs in a sandbox without network or access to kron. The page can call kron.action(name, context) and kron.set(path, value), and read kron.data; use CSS variables --fg, --bg, --surface, --accent, --line, --muted, --font, --mono for one look.

Actions (on Button, rowAction, itemAction):
- {"event": {"name": "verb_object", "context": {"key": literal or {"path": ...}}}} — comes back to you as a message `[ui-action] {"name", "context", "component"}`; respond by updating the interface (update_data / render_ui).
- {"functionCall": {"call": "set", "args": {"path": "/x", "value": ...}}} — local, instant (also "toggle" {path}, "openUrl" {url}, "copy" {text}).
- {"tool": {"name": "shell", "args": {"command": "..."}, "target": "/logs"}} — runs a tool directly on the machine when clicked, without you; output appended to the array at `target`. The user always sees the exact command and confirms anything that changes things. Use for repeatable operations (restart, run tests, git pull, refresh stats). Also "read_file" {file_path}, "list_dir" {dir_path}."#;

fn system_prompt(
    memory: &Memory,
    intent: &str,
    current: Option<&Surface>,
    template: Option<&profile::Template>,
) -> String {
    let mut s = String::from(
        "<generative_ui>\nYou are kron. The user does not chat with you in a chat window: you design and run a \
working interface for each task, for this specific person, and keep improving it.\n\n\
How to work:\n\
1. Decide what the user needs. For small talk or a short factual question, just answer in plain text (a minimal dialog) — \
no interface. For anything with structure, data, options, files, steps or controls, build an interface.\n\
2. Gather REAL data first with your tools (read files, list folders, run read-only commands, search the web). \
Never invent numbers, files, statuses or benchmarks; if something is unknown, say so in the interface. \
Be quick: the user is waiting for the interface. Look only at what it needs (a handful of tool calls, in parallel \
where possible), render, and then refine with update_data if you learn more.\n\
3. Call render_ui once with the complete interface. Prefer direct manipulation over text: filters, sorting, \
editable fields, checkboxes, reorderable lists and buttons, so the user never has to type what one tap does. \
Put data in `data` and bind to it; keep components few and meaningful (a dashboard, not a document).\n\
4. Follow-ups and [ui-action] messages refer to the current interface: change only what is needed. Use update_data \
for new values; call render_ui again only when the structure must change, and keep the layout the user is used to \
(same ids, order and placement) unless they ask otherwise.\n\
5. Mobile first: the interface must work on a phone at 360px width; Grid and Row adapt by themselves.\n\
6. Text in the interface is in the user's language. After render_ui, reply with at most one short sentence.\n\
7. Dangerous operations (deleting, deploying, restarting, paying, sending) go on a Button with variant \"danger\"; \
never run them yourself without the user's explicit request. Buttons with a tool action are confirmed by the user.\n\
8. If the user states a lasting preference about how they like to work or see things, call remember.\n\n",
    );
    s.push_str("<catalog>\n");
    s.push_str(CATALOG);
    s.push_str("\n</catalog>\n\n");
    s.push_str(&format!("Likely kind of task: {intent}. Kinds:"));
    for (name, about) in profile::INTENTS {
        s.push_str(&format!("\n- {name}: {about}"));
    }
    s.push('\n');
    let about = memory.describe(intent);
    if !about.is_empty() {
        s.push_str(&format!("\n<user_profile>\n{about}</user_profile>\n"));
    }
    if let Some(t) = template {
        let spec = json!({"title": t.title, "root": t.root, "components": t.components, "data": t.data});
        s.push_str(&format!(
            "\n<preferred_layout>\nThe user liked and used this layout for a similar task (\"{}\"). Build the new interface \
from it: keep its structure and component types, replace the content and data with the real ones for the new request.\n{}\n</preferred_layout>\n",
            t.title,
            clip(&spec.to_string(), 24_000)
        ));
    }
    if let Some(c) = current {
        let spec = json!({"title": c.title, "root": c.root, "components": c.components});
        s.push_str(&format!(
            "\n<current_interface version=\"{}\">\nThe user is looking at this interface (their own edits are in the data).\n\
components: {}\ndata: {}\n</current_interface>\n",
            c.version,
            clip(&spec.to_string(), 30_000),
            clip(&c.data.to_string(), 30_000)
        ));
    }
    s.push_str("</generative_ui>");
    s
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated)", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ask;

    struct Collect(Mutex<Vec<Event>>);
    impl Sink for Collect {
        fn emit(&self, e: Event) {
            self.0.lock().unwrap().push(e);
        }
        fn ask(&self, _: Ask) -> Option<tokio::sync::oneshot::Receiver<String>> {
            None
        }
    }

    fn tools(dir: &std::path::Path) -> UiTools {
        UiTools {
            surfaces: Surfaces::new(dir.join("s")),
            memory: Memory::new(dir.to_path_buf()),
            store: Store::new(dir.join("threads")),
            thread: "t".into(),
            request: "Сравни Claude и Codex".into(),
            intent: "compare".into(),
            template: None,
            current: Mutex::new(None),
            system: String::new(),
            failures: Mutex::new(0),
        }
    }

    #[tokio::test]
    async fn render_update_and_fallback() {
        let dir = std::env::temp_dir().join(format!("kron-g-{}", crate::types::new_id()));
        let t = tools(&dir);
        let sink = Collect(Mutex::new(vec![]));
        // Components sent as a JSON string are accepted too.
        let out = t
            .call("render_ui", &json!({"title": "Claude vs Codex", "components": "[{\"id\":\"root\",\"component\":\"Metric\",\"label\":\"x\",\"value\":{\"path\":\"/v\"}}]", "data": {"v": 1}}), &sink)
            .await
            .unwrap();
        assert!(out.starts_with("Rendered"), "{out}");
        let id = t.store.meta("t")["surface"].as_str().unwrap().to_string();
        assert_eq!(t.store.meta("t")["title"], "Claude vs Codex");
        let out = t.call("update_data", &json!({"patches": [{"path": "/v", "value": 2}]}), &sink).await.unwrap();
        assert!(out.contains("version 2"), "{out}");
        assert_eq!(t.surfaces.get(&id).unwrap().data["v"], 2);
        assert_eq!(sink.0.lock().unwrap().iter().filter(|e| matches!(e, Event::Surface { .. })).count(), 2);
        assert!(t.memory.profile().recent.iter().any(|r| r.surface == id));

        for i in 0..3 {
            let out = t
                .call("render_ui", &json!({"title": "x", "components": [{"id": "root", "component": "Nope"}]}), &sink)
                .await
                .unwrap();
            assert!(out.starts_with("error"));
            assert_eq!(out.contains("plain text"), i == 2, "{out}");
        }
        assert!(t.call("shell", &json!({}), &sink).await.is_none());
    }

    #[test]
    fn broken_arguments() {
        let v =
            object_args(&json!("{\"title\": \"x\", \"components\": [{\"id\": \"root\", \"text\": \"a\\\"b")).unwrap();
        assert_eq!(v["components"][0]["text"], "a\"b");
        assert!(object_args(&json!("{\"a\": 1 \"b\"")).is_err());
        assert!(object_args(&json!("[1]")).is_err());
        assert_eq!(object_args(&json!({"a": 1})).unwrap()["a"], 1);
    }

    #[test]
    fn actions_and_prompt() {
        let a = parse_action("[ui-action] {\"name\": \"refresh\", \"context\": {\"x\": 1}}").unwrap();
        assert_eq!(a["name"], "refresh");
        assert!(parse_action("hello").is_none());
        let dir = std::env::temp_dir().join(format!("kron-p-{}", crate::types::new_id()));
        let m = Memory::new(dir);
        m.remember("Prefers tables").unwrap();
        let p = system_prompt(&m, "compare", None, None);
        assert!(p.contains("Prefers tables") && p.contains("<catalog>") && p.contains("Likely kind of task: compare"));
        assert_eq!(clip("привет", 3), "п… (truncated)");
    }
}
