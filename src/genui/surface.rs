//! Surfaces: interfaces the agent builds for a task. The format follows A2UI
//! v0.9 (a flat list of components that point to their children by id, a
//! separate data model addressed by JSON Pointer, `{"path": ...}` bindings,
//! `{"event": ...}` actions) with kron's own component catalog.
//!
//! A surface lives in `~/.kron/genui/surfaces/<id>.json`; every change made by
//! the agent or the person is a version in `<id>.versions.jsonl`, so any of
//! them can be undone.
//!
//! A surface is alive without the model: `sources` are tools the hub runs on a
//! timer while someone looks at it (their results go into the data model),
//! components compute values with `{"expr": ...}` and show or hide with
//! `visible`, and `alerts` tell the person when a condition becomes true.

use crate::types::{new_id, now_ms};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Component name → properties that hold child ids. The catalog itself (what
/// each component does) is described to the model in `prompt::CATALOG`.
pub const CATALOG: &[(&str, &[&str])] = &[
    ("Column", &["children"]),
    ("Row", &["children"]),
    ("Grid", &["children"]),
    ("Card", &["children"]),
    ("Tabs", &[]),
    ("Collapsible", &["children"]),
    ("Divider", &[]),
    ("Heading", &[]),
    ("Text", &[]),
    ("Badge", &[]),
    ("Metric", &[]),
    ("Progress", &[]),
    ("KeyValue", &[]),
    ("Code", &[]),
    ("Diff", &[]),
    ("Log", &[]),
    ("Image", &[]),
    ("Table", &[]),
    ("Chart", &[]),
    ("List", &[]),
    ("Tree", &[]),
    ("TextField", &[]),
    ("Select", &[]),
    ("ChoicePicker", &[]),
    ("CheckBox", &[]),
    ("Slider", &[]),
    ("Button", &[]),
    ("Editor", &[]),
    ("Terminal", &[]),
    ("Html", &[]),
];

const MAX_COMPONENTS: usize = 400;
const MAX_BYTES: usize = 600_000;
const MAX_VERSIONS: usize = 60;
const MAX_AUDIT: usize = 100;
const MAX_SOURCES: usize = 20;
const MAX_ALERTS: usize = 20;
/// Tools a live source may run: reading, or a shell command the person allowed.
pub const LIVE_TOOLS: &[&str] = &["shell", "read_file", "list_dir", "grep_files"];
const PARSE: &[&str] = &["text", "lines", "number", "json", "table", "kv"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Surface {
    pub id: String,
    pub thread: String,
    pub title: String,
    pub intent: String,
    /// The request that created it (used to find it again).
    pub request: String,
    pub root: String,
    pub components: Vec<Value>,
    pub data: Value,
    pub version: u64,
    pub created: i64,
    pub updated: i64,
    pub pinned: bool,
    /// Was built from a remembered layout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// How the person used it: interactions, likes, dislikes.
    pub stats: Stats,
    /// Tools run from the surface's buttons and terminal, newest last.
    pub audit: Vec<Value>,
    /// Live data: `{id, tool: {name, args}, target, every, parse, keep?}`.
    pub sources: Vec<Value>,
    /// `{id, when: expression, text, tone}`: shown (and notified) while true.
    pub alerts: Vec<Value>,
    /// Source commands the person allowed to run without asking (`source_key`).
    pub approved: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    pub interactions: u64,
    pub opens: u64,
    pub likes: u64,
    pub dislikes: u64,
    /// Component type → times the person used one.
    pub used: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub version: u64,
    pub ts: i64,
    /// agent | user | undo
    pub by: String,
    pub note: String,
    pub title: String,
    pub root: String,
    pub components: Vec<Value>,
    pub data: Value,
    #[serde(default)]
    pub sources: Vec<Value>,
    #[serde(default)]
    pub alerts: Vec<Value>,
}

/// A short entry for lists.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub id: String,
    pub thread: String,
    pub title: String,
    pub intent: String,
    pub updated: i64,
    pub pinned: bool,
    pub version: u64,
    pub score: i64,
}

impl Surface {
    pub fn summary(&self) -> Summary {
        Summary {
            id: self.id.clone(),
            thread: self.thread.clone(),
            title: self.title.clone(),
            intent: self.intent.clone(),
            updated: self.updated,
            pinned: self.pinned,
            version: self.version,
            score: self.stats.interactions as i64 + 5 * self.stats.likes as i64 - 5 * self.stats.dislikes as i64,
        }
    }

    /// What the client renders.
    pub fn to_client(&self) -> Value {
        json!({
            "id": self.id, "thread": self.thread, "title": self.title, "intent": self.intent,
            "root": self.root, "components": self.components, "data": self.data,
            "version": self.version, "updated": self.updated, "pinned": self.pinned,
            "template": self.template, "audit": self.audit,
            "sources": self.sources, "alerts": self.alerts,
        })
    }
}

/// Every read-modify-write of a surface goes through this lock (agent and
/// person can change the same surface at once).
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone)]
pub struct Surfaces {
    dir: PathBuf,
}

fn valid_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 80 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("invalid surface id '{id}'");
    }
    Ok(())
}

impl Surfaces {
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).ok();
        Surfaces { dir }
    }

    fn file(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn versions_file(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.versions.jsonl"))
    }

    pub fn get(&self, id: &str) -> Result<Surface> {
        valid_id(id)?;
        let text = std::fs::read_to_string(self.file(id)).map_err(|_| anyhow::anyhow!("no surface '{id}'"))?;
        Ok(serde_json::from_str(&text)?)
    }

    fn put(&self, s: &Surface) -> Result<()> {
        let tmp = self.file(&s.id).with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string(s)?)?;
        std::fs::rename(tmp, self.file(&s.id))?;
        Ok(())
    }

    pub fn list(&self) -> Vec<Summary> {
        let mut out: Vec<Summary> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let id = name.strip_suffix(".json")?;
                self.get(id).ok().map(|s| s.summary())
            })
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.updated));
        out
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        valid_id(id)?;
        std::fs::remove_file(self.file(id))?;
        std::fs::remove_file(self.versions_file(id)).ok();
        Ok(())
    }

    /// Create or replace the structure. `data` is merged into the current data
    /// (top-level keys) unless `replace_data`.
    pub fn render(&self, id: Option<&str>, thread: &str, spec: &Value, by: &str) -> Result<Surface> {
        let _g = LOCK.lock().unwrap();
        let checked = validate(spec)?;
        let mut s = match id {
            Some(id) => self.get(id)?,
            None => Surface {
                id: format!("s-{}", new_id()),
                thread: thread.to_string(),
                created: now_ms(),
                ..Default::default()
            },
        };
        if let Some(t) = spec["title"].as_str().filter(|t| !t.trim().is_empty()) {
            s.title = t.trim().chars().take(120).collect();
        }
        if let Some(i) = spec["intent"].as_str().filter(|t| !t.trim().is_empty()) {
            s.intent = i.trim().to_lowercase().chars().take(40).collect();
        }
        s.root = checked.root;
        s.components = checked.components;
        if let Some(x) = checked.sources {
            s.sources = x;
        }
        if let Some(x) = checked.alerts {
            s.alerts = x;
        }
        merge_data(&mut s, spec)?;
        self.commit(&mut s, by, spec["note"].as_str().unwrap_or(""))?;
        Ok(s)
    }

    /// Change part of the structure: `set` replaces or adds components by id,
    /// `remove` drops them, `sources` replaces or adds sources by id,
    /// `remove_sources` drops them, `alerts` replaces the list. The result is
    /// checked as a whole, and components nothing shows any more are dropped.
    pub fn patch(&self, id: &str, spec: &Value, by: &str) -> Result<Surface> {
        let _g = LOCK.lock().unwrap();
        let mut s = self.get(id)?;
        let ids = |v: &Value| -> HashSet<String> {
            v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
        };
        let mut components = s.components.clone();
        upsert(&mut components, &spec["set"], "set")?;
        let gone = ids(&spec["remove"]);
        components.retain(|c| !gone.contains(c["id"].as_str().unwrap_or("")));
        let mut sources = s.sources.clone();
        upsert(&mut sources, &spec["sources"], "sources")?;
        let gone = ids(&spec["remove_sources"]);
        sources.retain(|c| !gone.contains(c["id"].as_str().unwrap_or("")));
        let alerts = spec.get("alerts").filter(|a| !a.is_null()).cloned().unwrap_or(json!(s.alerts));
        let root = spec["root"].as_str().unwrap_or(&s.root).to_string();
        let checked = validate(&json!({"root": root, "components": components, "sources": sources, "alerts": alerts}))?;
        s.root = checked.root;
        s.components = reachable(&s.root, checked.components);
        s.sources = checked.sources.unwrap_or_default();
        s.alerts = checked.alerts.unwrap_or_default();
        if let Some(t) = spec["title"].as_str().filter(|t| !t.trim().is_empty()) {
            s.title = t.trim().chars().take(120).collect();
        }
        merge_data(&mut s, spec)?;
        self.commit(&mut s, by, spec["note"].as_str().unwrap_or(""))?;
        Ok(s)
    }

    /// The person rearranges the interface: `move` a component one place
    /// among its siblings (`dir` -1 or 1), or `hide` it.
    pub fn layout(&self, id: &str, op: &str, target: &str, dir: i64) -> Result<Surface> {
        let _g = LOCK.lock().unwrap();
        let mut s = self.get(id)?;
        if target == s.root {
            bail!("the root of the interface cannot be moved or hidden");
        }
        let mut done = false;
        'find: for c in s.components.iter_mut() {
            for list in sibling_lists(c) {
                let Some(i) = list.iter().position(|x| x.as_str() == Some(target)) else { continue };
                match op {
                    "hide" => {
                        list.remove(i);
                    }
                    "move" => {
                        let j = i as i64 + dir.signum();
                        if j < 0 || j as usize >= list.len() {
                            bail!("'{target}' is already at the edge");
                        }
                        list.swap(i, j as usize);
                    }
                    _ => bail!("unknown layout operation '{op}'"),
                }
                done = true;
                break 'find;
            }
        }
        if !done {
            bail!("'{target}' is not in a list of components that can be rearranged");
        }
        s.components = reachable(&s.root, std::mem::take(&mut s.components));
        let note = if op == "hide" { format!("hid {target}") } else { format!("moved {target}") };
        self.commit(&mut s, "user", &note)?;
        Ok(s)
    }

    /// Let these sources run without asking each time.
    pub fn approve(&self, id: &str, sources: &[String]) -> Result<Surface> {
        let _g = LOCK.lock().unwrap();
        let mut s = self.get(id)?;
        for src in s.sources.iter().filter(|x| sources.iter().any(|id| x["id"] == id.as_str())) {
            let key = source_key(src);
            if !s.approved.contains(&key) {
                s.approved.push(key);
            }
        }
        let keep = s.approved.len().saturating_sub(100);
        s.approved.drain(..keep);
        self.put(&s)?;
        Ok(s)
    }

    /// A live source's new value goes into the data model. Not a version: it
    /// changes every few seconds, and undo is for what people and the agent did.
    /// `keep` sources append `{t, ts, ...}` and keep the last `keep` entries.
    pub fn write_live(&self, id: &str, source: &Value, value: Value, label: &str) -> Result<Value> {
        let _g = LOCK.lock().unwrap();
        let mut s = self.get(id)?;
        let target = source["target"].as_str().unwrap_or("/live");
        let keep = source["keep"].as_u64().unwrap_or(0) as usize;
        if keep > 0 {
            let mut point = match value {
                Value::Object(m) => Value::Object(m),
                v => json!({"value": v}),
            };
            point["t"] = json!(label);
            point["ts"] = json!(now_ms());
            let mut list = s.data.pointer(target).and_then(Value::as_array).cloned().unwrap_or_default();
            list.push(point);
            let cut = list.len().saturating_sub(keep);
            list.drain(..cut);
            set_path(&mut s.data, target, Some(Value::Array(list)))?;
        } else {
            set_path(&mut s.data, target, Some(value))?;
        }
        if serde_json::to_string(&s.data)?.len() > MAX_BYTES {
            bail!("data model is too large");
        }
        self.put(&s)?;
        Ok(s.data.pointer(target).cloned().unwrap_or(Value::Null))
    }

    /// Change the data model at JSON Pointer paths. A missing or null `value` removes the key.
    pub fn patch_data(&self, id: &str, patches: &[(String, Option<Value>)], by: &str) -> Result<Surface> {
        let _g = LOCK.lock().unwrap();
        let mut s = self.get(id)?;
        for (path, value) in patches {
            set_path(&mut s.data, path, value.clone())?;
        }
        if serde_json::to_string(&s.data)?.len() > MAX_BYTES {
            bail!("data model is too large");
        }
        let note = patches.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(", ");
        // A person typing makes many small edits: one version per field per half a minute.
        let coalesce = by == "user"
            && self.last_version(id).is_some_and(|v| v.by == "user" && v.note == note && now_ms() - v.ts < 30_000);
        if coalesce {
            s.updated = now_ms();
            self.put(&s)?;
            self.rewrite_last_version(&s)?;
        } else {
            self.commit(&mut s, by, &note)?;
        }
        Ok(s)
    }

    fn commit(&self, s: &mut Surface, by: &str, note: &str) -> Result<()> {
        s.version += 1;
        s.updated = now_ms();
        self.put(s)?;
        let v = Version {
            version: s.version,
            ts: s.updated,
            by: by.into(),
            note: note.chars().take(200).collect(),
            title: s.title.clone(),
            root: s.root.clone(),
            components: s.components.clone(),
            data: s.data.clone(),
            sources: s.sources.clone(),
            alerts: s.alerts.clone(),
        };
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.versions_file(&s.id))?;
        writeln!(f, "{}", serde_json::to_string(&v)?)?;
        drop(f);
        let all = self.versions(&s.id);
        if all.len() > MAX_VERSIONS + 10 {
            self.write_versions(&s.id, &all[all.len() - MAX_VERSIONS..])?;
        }
        Ok(())
    }

    pub fn versions(&self, id: &str) -> Vec<Version> {
        std::fs::read_to_string(self.versions_file(id))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn last_version(&self, id: &str) -> Option<Version> {
        self.versions(id).pop()
    }

    fn write_versions(&self, id: &str, list: &[Version]) -> Result<()> {
        let text: String = list.iter().map(|v| serde_json::to_string(v).unwrap() + "\n").collect();
        let tmp = self.versions_file(id).with_extension("jsonl.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, self.versions_file(id))?;
        Ok(())
    }

    fn rewrite_last_version(&self, s: &Surface) -> Result<()> {
        let mut all = self.versions(&s.id);
        if let Some(last) = all.last_mut() {
            last.ts = s.updated;
            last.data = s.data.clone();
        }
        self.write_versions(&s.id, &all)
    }

    /// Go back to `version` (default: the one before the current). The undo is a new version itself.
    pub fn restore(&self, id: &str, version: Option<u64>) -> Result<Surface> {
        let _g = LOCK.lock().unwrap();
        let mut s = self.get(id)?;
        let all = self.versions(id);
        let target = match version {
            Some(v) => all.iter().rev().find(|x| x.version == v),
            None => all.iter().rev().find(|x| x.version < s.version),
        };
        let Some(t) = target.cloned() else { bail!("nothing to undo") };
        s.title = t.title;
        s.root = t.root;
        s.components = t.components;
        s.data = t.data;
        s.sources = t.sources;
        s.alerts = t.alerts;
        self.commit(&mut s, "undo", &format!("restored v{}", t.version))?;
        Ok(s)
    }

    /// Small changes that are not versions: pin, stats, audit, title.
    pub fn update(&self, id: &str, f: impl FnOnce(&mut Surface)) -> Result<Surface> {
        let _g = LOCK.lock().unwrap();
        let mut s = self.get(id)?;
        f(&mut s);
        if s.audit.len() > MAX_AUDIT {
            let cut = s.audit.len() - MAX_AUDIT;
            s.audit.drain(..cut);
        }
        self.put(&s)?;
        Ok(s)
    }
}

/// A surface that passed validation.
#[derive(Debug)]
pub struct Checked {
    pub root: String,
    pub components: Vec<Value>,
    /// Present when the spec had them.
    pub sources: Option<Vec<Value>>,
    pub alerts: Option<Vec<Value>>,
}

/// Check a render request: known components, unique ids, children that exist,
/// one root, no cycles, sane sizes. Errors are written for the model to fix.
pub fn validate(spec: &Value) -> Result<Checked> {
    let Some(list) = spec["components"].as_array() else {
        bail!("'components' must be an array of component objects");
    };
    if list.is_empty() {
        bail!("'components' is empty");
    }
    if list.len() > MAX_COMPONENTS {
        bail!(
            "too many components ({}, max {MAX_COMPONENTS}); use Table/List/Chart with data instead of one component per item",
            list.len()
        );
    }
    if serde_json::to_string(list)?.len() > MAX_BYTES {
        bail!("components are too large; move repeated content into the data model");
    }
    let known: HashMap<&str, &[&str]> = CATALOG.iter().copied().collect();
    let mut errors = Vec::new();
    let mut ids = HashSet::new();
    let mut out = Vec::new();
    for (i, c) in list.iter().enumerate() {
        let Some(obj) = c.as_object() else {
            errors.push(format!("components[{i}] is not an object"));
            continue;
        };
        let id = obj.get("id").and_then(Value::as_str).unwrap_or("");
        if id.is_empty() {
            errors.push(format!("components[{i}] has no 'id'"));
            continue;
        }
        let kind = component_name(c);
        if kind.is_empty() {
            errors.push(format!("component '{id}' has no 'component' name"));
            continue;
        }
        if !known.contains_key(kind.as_str()) {
            let names: Vec<&str> = CATALOG.iter().map(|(n, _)| *n).collect();
            errors.push(format!("component '{id}': unknown type '{kind}'. Use one of: {}", names.join(", ")));
            continue;
        }
        if !ids.insert(id.to_string()) {
            errors.push(format!("duplicate component id '{id}'"));
            continue;
        }
        let mut norm = normalize(obj, &kind);
        if kind == "Html" && norm["html"].as_str().is_none_or(|h| h.trim().is_empty()) {
            errors.push(format!("Html '{id}' needs 'html'"));
        }
        if kind == "Button" && norm.get("label").is_none() && norm.get("icon").is_none() {
            norm["label"] = json!("OK");
        }
        out.push(norm);
    }
    let sources = spec.get("sources").filter(|v| !v.is_null()).map(|v| check_sources(v, &mut errors));
    let alerts = spec.get("alerts").filter(|v| !v.is_null()).map(|v| check_alerts(v, &mut errors));
    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }

    // Children: drop references to ids that do not exist (a warning, not an error).
    let mut parent_of: HashMap<String, String> = HashMap::new();
    for c in out.iter_mut() {
        let id = c["id"].as_str().unwrap_or("").to_string();
        for child in child_ids_mut(c, &ids) {
            parent_of.entry(child).or_insert(id.clone());
        }
    }
    let root = spec["root"]
        .as_str()
        .filter(|r| ids.contains(*r))
        .map(String::from)
        .or_else(|| ids.contains("root").then(|| "root".to_string()))
        .or_else(|| {
            let tops: Vec<&Value> =
                out.iter().filter(|c| !parent_of.contains_key(c["id"].as_str().unwrap_or(""))).collect();
            (tops.len() == 1).then(|| tops[0]["id"].as_str().unwrap_or("").to_string())
        });
    let Some(root) = root else {
        bail!("no root: give the top component the id 'root' (a Column is a good root)");
    };
    // Cycles: walk from the root.
    let by_id: HashMap<String, &Value> = out.iter().map(|c| (c["id"].as_str().unwrap_or("").to_string(), c)).collect();
    let mut stack = vec![(root.clone(), 0usize, vec![root.clone()])];
    while let Some((id, depth, trail)) = stack.pop() {
        if depth > 40 {
            bail!("components nest deeper than 40 levels");
        }
        let Some(c) = by_id.get(&id) else { continue };
        for child in child_refs(c) {
            if trail.contains(&child) {
                bail!("component '{child}' contains itself (cycle via '{id}')");
            }
            let mut t = trail.clone();
            t.push(child.clone());
            stack.push((child, depth + 1, t));
        }
    }
    Ok(Checked { root, components: out, sources, alerts })
}

/// What the person allowed when they approved a source: its exact tool call.
pub fn source_key(source: &Value) -> String {
    source["tool"].to_string()
}

fn check_sources(v: &Value, errors: &mut Vec<String>) -> Vec<Value> {
    let Some(list) = v.as_array() else {
        errors.push("'sources' must be an array".into());
        return vec![];
    };
    if list.len() > MAX_SOURCES {
        errors.push(format!("too many sources ({}, max {MAX_SOURCES})", list.len()));
    }
    let mut ids = HashSet::new();
    let mut out = Vec::new();
    for (i, s) in list.iter().take(MAX_SOURCES).enumerate() {
        let id = s["id"].as_str().map(String::from).unwrap_or_else(|| format!("src{i}"));
        if !ids.insert(id.clone()) {
            errors.push(format!("duplicate source id '{id}'"));
            continue;
        }
        // Shorthand: {"command": "df -h"} is a shell source.
        let tool = match s["command"].as_str() {
            Some(c) => json!({"name": "shell", "args": {"command": c}}),
            None => s["tool"].clone(),
        };
        let name = tool["name"].as_str().unwrap_or("");
        if !LIVE_TOOLS.contains(&name) {
            errors.push(format!("source '{id}': tool must be one of {}", LIVE_TOOLS.join(", ")));
            continue;
        }
        if !tool["args"].is_object() {
            errors.push(format!("source '{id}': tool.args must be an object"));
            continue;
        }
        let Some(target) = s["target"].as_str().filter(|t| t.starts_with('/') && t.len() > 1) else {
            errors.push(format!("source '{id}' needs 'target', a data path such as /cpu"));
            continue;
        };
        let parse = s["parse"].as_str().unwrap_or("text");
        if !PARSE.contains(&parse) {
            errors.push(format!("source '{id}': parse must be one of {}", PARSE.join(", ")));
            continue;
        }
        let every = s["every"].as_f64().unwrap_or(0.0);
        let every = if every <= 0.0 { 0.0 } else { every.clamp(2.0, 86_400.0) };
        let mut src = json!({"id": id, "tool": {"name": name, "args": tool["args"]}, "target": target,
            "every": every, "parse": parse});
        if let Some(k) = s["keep"].as_u64().filter(|k| *k > 0) {
            src["keep"] = json!(k.min(500));
        }
        if let Some(l) = s["label"].as_str() {
            src["label"] = json!(l);
        }
        out.push(src);
    }
    out
}

fn check_alerts(v: &Value, errors: &mut Vec<String>) -> Vec<Value> {
    let Some(list) = v.as_array() else {
        errors.push("'alerts' must be an array".into());
        return vec![];
    };
    let mut out = Vec::new();
    for (i, a) in list.iter().take(MAX_ALERTS).enumerate() {
        let id = a["id"].as_str().map(String::from).unwrap_or_else(|| format!("alert{i}"));
        let Some(when) = a["when"].as_str().filter(|w| !w.trim().is_empty() && w.len() <= 1000) else {
            errors.push(format!("alert '{id}' needs 'when', an expression such as \"/cpu > 90\""));
            continue;
        };
        out.push(json!({"id": id, "when": when, "text": a["text"], "tone": a["tone"].as_str().unwrap_or("warn")}));
    }
    out
}

/// A tool's output as a live value. Failures (a non-zero exit, a tool
/// error, output that does not parse) are errors and leave the data as it was.
pub fn parse_output(tool: &str, parse: &str, output: &str) -> Result<Value> {
    let text = if tool == "shell" {
        let (head, rest) = output.split_once('\n').unwrap_or((output, ""));
        let code = head.strip_prefix("Exit code: ").and_then(|c| c.trim().parse::<i64>().ok());
        let body = rest.strip_prefix("Output:\n").or(rest.strip_prefix("Output:")).unwrap_or(rest);
        match code {
            Some(0) => body.to_string(),
            Some(c) => bail!("exit code {c}: {}", crate::types::preview(body.trim(), 300)),
            None => bail!("{}", crate::types::preview(output.trim(), 300)),
        }
    } else if output.starts_with("error") {
        bail!("{}", crate::types::preview(output.trim(), 300));
    } else {
        output.to_string()
    };
    let text = text.trim_end();
    Ok(match parse {
        "lines" => json!(text.lines().filter(|l| !l.trim().is_empty()).take(1000).collect::<Vec<_>>()),
        "number" => {
            let n = first_number(text).ok_or_else(|| anyhow::anyhow!("no number in the output"))?;
            json!(n)
        }
        "json" => serde_json::from_str(text.trim()).map_err(|e| anyhow::anyhow!("output is not JSON: {e}"))?,
        "table" => {
            let mut lines = text.lines().filter(|l| !l.trim().is_empty());
            let head: Vec<&str> = lines.next().unwrap_or("").split_whitespace().collect();
            let rows: Vec<Value> = lines
                .take(1000)
                .map(|l| {
                    let cells = split_n(l, head.len());
                    Value::Object(head.iter().zip(cells).map(|(k, v)| (k.to_string(), cell(v))).collect())
                })
                .collect();
            json!(rows)
        }
        "kv" => Value::Object(
            text.lines()
                .filter_map(|l| {
                    let (k, v) = l.split_once(':').or_else(|| l.split_once('='))?;
                    let k = k.trim();
                    (!k.is_empty()).then(|| (k.to_string(), cell(v.trim())))
                })
                .collect(),
        ),
        _ => json!(text),
    })
}

/// A table cell: a number when it is one ("12", "3.5", "40%" stays text).
fn cell(v: &str) -> Value {
    v.parse::<i64>().map(|n| json!(n)).or_else(|_| v.parse::<f64>().map(|n| json!(n))).unwrap_or(json!(v))
}

/// Split on whitespace into at most `n` parts; the last part keeps the rest of the line.
fn split_n(line: &str, n: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        if out.len() + 1 >= n.max(1) {
            out.push(rest);
            break;
        }
        match rest.find(char::is_whitespace) {
            Some(i) => {
                out.push(&rest[..i]);
                rest = rest[i..].trim_start();
            }
            None => {
                out.push(rest);
                break;
            }
        }
    }
    out
}

fn first_number(text: &str) -> Option<f64> {
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let neg = c == '-' && bytes.get(i + 1).is_some_and(|d| d.is_ascii_digit());
        if c.is_ascii_digit() || neg {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || ((bytes[i] == '.' || bytes[i] == ',') && bytes.get(i + 1).is_some_and(|d| d.is_ascii_digit())))
            {
                i += 1;
            }
            let s: String = bytes[start..i].iter().collect();
            return s.replace(',', ".").parse().ok();
        }
        i += 1;
    }
    None
}

/// Replace items with the same id, append new ones.
fn upsert(list: &mut Vec<Value>, new: &Value, what: &str) -> Result<()> {
    let items = match new {
        Value::Null => return Ok(()),
        Value::Array(a) => a,
        _ => bail!("'{what}' must be an array of objects with an 'id'"),
    };
    for item in items {
        let Some(id) = item["id"].as_str() else { bail!("every item of '{what}' needs an 'id'") };
        match list.iter().position(|x| x["id"] == id) {
            Some(i) => list[i] = item.clone(),
            None => list.push(item.clone()),
        }
    }
    Ok(())
}

/// Only the components the root can reach.
fn reachable(root: &str, components: Vec<Value>) -> Vec<Value> {
    let by_id: HashMap<&str, &Value> = components.iter().map(|c| (c["id"].as_str().unwrap_or(""), c)).collect();
    let mut seen = HashSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(c) = by_id.get(id.as_str()) {
            stack.extend(child_refs(c));
        }
    }
    components.into_iter().filter(|c| seen.contains(c["id"].as_str().unwrap_or(""))).collect()
}

/// The child id lists of a component, to rearrange.
fn sibling_lists(c: &mut Value) -> Vec<&mut Vec<Value>> {
    let mut out = Vec::new();
    let Some(o) = c.as_object_mut() else { return out };
    for (k, v) in o.iter_mut() {
        match (k.as_str(), v) {
            ("children", Value::Array(a)) => out.push(a),
            ("tabs", Value::Array(tabs)) => {
                for t in tabs {
                    if let Some(Value::Array(a)) = t.get_mut("children") {
                        out.push(a);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Merge `spec.data` into the data model (top-level keys), or replace it with `replace_data`.
fn merge_data(s: &mut Surface, spec: &Value) -> Result<()> {
    let replace = spec["replace_data"].as_bool().unwrap_or(false);
    match (&mut s.data, spec.get("data")) {
        (_, None) | (_, Some(Value::Null)) => {}
        (Value::Object(cur), Some(Value::Object(new))) if !replace => {
            for (k, v) in new {
                cur.insert(k.clone(), v.clone());
            }
        }
        (cur, Some(new)) => *cur = new.clone(),
    }
    if !s.data.is_object() {
        s.data = json!({});
    }
    if serde_json::to_string(&s.data)?.len() > MAX_BYTES {
        bail!("data model is too large (over {} KB); keep only what the interface shows", MAX_BYTES / 1000);
    }
    Ok(())
}

/// `"component": "Text"` (v0.9) or `"component": {"Text": {...}}` (v0.8).
fn component_name(c: &Value) -> String {
    match &c["component"] {
        Value::String(s) => s.clone(),
        Value::Object(m) if m.len() == 1 => m.keys().next().cloned().unwrap_or_default(),
        _ => c["type"].as_str().unwrap_or("").to_string(),
    }
}

/// Bring both A2UI shapes to the flat v0.9 one.
fn normalize(obj: &Map<String, Value>, kind: &str) -> Value {
    let mut out = Map::new();
    if let Some(Value::Object(m)) = obj.get("component")
        && let Some(Value::Object(props)) = m.get(kind)
    {
        for (k, v) in props {
            out.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in obj {
        if k != "component" && k != "type" {
            out.insert(k.clone(), v.clone());
        }
    }
    out.insert("component".into(), json!(kind));
    Value::Object(out)
}

/// Child ids of a component (static lists, tab contents and template components).
pub fn child_refs(c: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut take = |v: &Value| match v {
        Value::Array(a) => out.extend(a.iter().filter_map(|x| x.as_str().map(String::from))),
        Value::Object(o) => {
            if let Some(t) = o.get("componentId").and_then(Value::as_str) {
                out.push(t.to_string())
            }
        }
        _ => {}
    };
    take(&c["children"]);
    if let Some(tabs) = c["tabs"].as_array() {
        for t in tabs {
            take(&t["children"]);
        }
    }
    out
}

/// Remove references to unknown ids; returns the remaining child ids.
fn child_ids_mut(c: &mut Value, ids: &HashSet<String>) -> Vec<String> {
    let mut found = Vec::new();
    let mut fix = |v: &mut Value| {
        if let Value::Array(a) = v {
            a.retain(|x| x.as_str().is_some_and(|s| ids.contains(s)));
            found.extend(a.iter().filter_map(|x| x.as_str().map(String::from)));
        } else if let Value::Object(o) = v
            && let Some(t) = o.get("componentId").and_then(Value::as_str)
        {
            if ids.contains(t) {
                found.push(t.to_string());
            } else {
                *v = json!([]);
            }
        }
    };
    if c.get("children").is_some() {
        fix(&mut c["children"]);
    }
    if let Some(tabs) = c.get_mut("tabs").and_then(Value::as_array_mut) {
        for t in tabs {
            if t.get("children").is_some() {
                fix(&mut t["children"]);
            }
        }
    }
    found
}

fn unescape(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

/// Set (or with None remove) the value at a JSON Pointer, creating objects on
/// the way. `-` appends to an array.
pub fn set_path(root: &mut Value, path: &str, value: Option<Value>) -> Result<()> {
    if path.is_empty() || path == "/" {
        *root = value.unwrap_or(json!({}));
        return Ok(());
    }
    let Some(rest) = path.strip_prefix('/') else { bail!("path '{path}' must start with '/'") };
    let tokens: Vec<String> = rest.split('/').map(unescape).collect();
    let mut cur = root;
    for (i, t) in tokens.iter().enumerate() {
        let last = i == tokens.len() - 1;
        if cur.is_null() {
            *cur = if t.parse::<usize>().is_ok() || t == "-" { json!([]) } else { json!({}) };
        }
        match cur {
            Value::Object(m) => {
                if last {
                    match value {
                        Some(v) => {
                            m.insert(t.clone(), v);
                        }
                        None => {
                            m.remove(t);
                        }
                    }
                    return Ok(());
                }
                cur = m.entry(t.clone()).or_insert(Value::Null);
            }
            Value::Array(a) => {
                let idx = if t == "-" {
                    a.len()
                } else {
                    t.parse::<usize>().map_err(|_| anyhow::anyhow!("'{t}' in '{path}' is not an array index"))?
                };
                if last {
                    match value {
                        Some(v) if idx >= a.len() => a.push(v),
                        Some(v) => a[idx] = v,
                        None if idx < a.len() => {
                            a.remove(idx);
                        }
                        None => {}
                    }
                    return Ok(());
                }
                if idx >= a.len() {
                    bail!("index {idx} in '{path}' is out of range");
                }
                cur = &mut a[idx];
            }
            _ => bail!("'{path}' goes through a value that is not an object or array"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Surfaces {
        Surfaces::new(std::env::temp_dir().join(format!("kron-s-{}", new_id())))
    }

    #[test]
    fn validates_and_normalizes() {
        let spec = json!({"components": [
            {"id": "root", "component": "Column", "children": ["t", "ghost"]},
            {"id": "t", "component": {"Text": {"text": "hi"}}},
        ]});
        let c = validate(&spec).unwrap();
        assert_eq!(c.root, "root");
        assert_eq!(c.components[0]["children"], json!(["t"]));
        assert_eq!(c.components[1]["component"], "Text");
        assert_eq!(c.components[1]["text"], "hi");

        let bad = json!({"components": [{"id": "root", "component": "Spaceship"}]});
        assert!(validate(&bad).unwrap_err().to_string().contains("unknown type 'Spaceship'"));
        let cycle = json!({"components": [
            {"id": "root", "component": "Column", "children": ["a"]},
            {"id": "a", "component": "Card", "children": ["root"]},
        ]});
        assert!(validate(&cycle).unwrap_err().to_string().contains("cycle"));
        let dup = json!({"components": [{"id": "x", "component": "Text"}, {"id": "x", "component": "Text"}]});
        assert!(validate(&dup).is_err());
        // A single top component is the root even without the name.
        let one = json!({"components": [{"id": "main", "component": "Column", "children": ["a"]}, {"id": "a", "component": "Text"}]});
        assert_eq!(validate(&one).unwrap().root, "main");
    }

    #[test]
    fn pointer_set() {
        let mut v = json!({});
        set_path(&mut v, "/a/b", Some(json!(1))).unwrap();
        set_path(&mut v, "/list/-", Some(json!("x"))).unwrap();
        set_path(&mut v, "/list/-", Some(json!("y"))).unwrap();
        set_path(&mut v, "/list/0", None).unwrap();
        set_path(&mut v, "/a~1b", Some(json!(true))).unwrap();
        assert_eq!(v, json!({"a": {"b": 1}, "list": ["y"], "a/b": true}));
        assert!(set_path(&mut v, "/a/b/c", Some(json!(1))).is_err());
    }

    #[test]
    fn versions_and_undo() {
        let s = store();
        let spec = json!({"title": "One", "components": [{"id": "root", "component": "Text", "text": {"path": "/x"}}], "data": {"x": "a", "keep": 1}});
        let a = s.render(None, "t1", &spec, "agent").unwrap();
        assert_eq!(a.version, 1);
        let b = s.patch_data(&a.id, &[("/x".into(), Some(json!("b")))], "user").unwrap();
        assert_eq!(b.version, 2);
        // Typing into the same field again within 30 s is the same version.
        let b2 = s.patch_data(&a.id, &[("/x".into(), Some(json!("bc")))], "user").unwrap();
        assert_eq!(b2.version, 2);
        assert_eq!(s.versions(&a.id).last().unwrap().data["x"], "bc");
        // Re-rendering merges data by default.
        let c = s
            .render(
                Some(&a.id),
                "t1",
                &json!({"components": [{"id": "root", "component": "Text"}], "data": {"y": 2}}),
                "agent",
            )
            .unwrap();
        assert_eq!(c.data, json!({"x": "bc", "keep": 1, "y": 2}));
        let u = s.restore(&a.id, None).unwrap();
        assert_eq!(u.data, json!({"x": "bc", "keep": 1}));
        assert_eq!(u.version, 4);
        let first = s.restore(&a.id, Some(1)).unwrap();
        assert_eq!(first.data["x"], "a");
        assert_eq!(s.list().len(), 1);
        s.delete(&a.id).unwrap();
        assert!(s.get(&a.id).is_err());
    }

    #[test]
    fn patch_and_layout() {
        let s = store();
        let spec = json!({"title": "P", "components": [
            {"id": "root", "component": "Column", "children": ["a", "b", "c"]},
            {"id": "a", "component": "Text", "text": "a"},
            {"id": "b", "component": "Card", "children": ["b1"]},
            {"id": "b1", "component": "Text", "text": "b1"},
            {"id": "c", "component": "Text", "text": "c"},
        ], "sources": [{"id": "up", "command": "uptime", "target": "/up", "every": 1}]});
        let a = s.render(None, "t", &spec, "agent").unwrap();
        assert_eq!(a.sources[0]["tool"], json!({"name": "shell", "args": {"command": "uptime"}}));
        assert_eq!(a.sources[0]["every"], 2.0);

        // Replace one component, add one, remove one: the rest stays.
        let p = s
            .patch(
                &a.id,
                &json!({"set": [{"id": "a", "component": "Badge", "text": "new"},
                    {"id": "root", "component": "Column", "children": ["a", "b", "c", "d"]},
                    {"id": "d", "component": "Metric", "label": "x", "value": {"expr": "1 + 1"}}],
                    "remove": ["c"], "sources": [{"id": "disk", "command": "df -k /", "target": "/disk", "parse": "table"}]}),
                "agent",
            )
            .unwrap();
        let ids: Vec<&str> = p.components.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["root", "a", "b", "b1", "d"]);
        assert_eq!(p.components[1]["component"], "Badge");
        assert_eq!(p.components[0]["children"], json!(["a", "b", "d"]));
        assert_eq!(p.sources.len(), 2);
        assert!(s.patch(&a.id, &json!({"set": [{"id": "x", "component": "Nope"}]}), "agent").is_err());
        assert!(
            s.patch(
                &a.id,
                &json!({"sources": [{"id": "bad", "tool": {"name": "apply_patch", "args": {}}, "target": "/x"}]}),
                "agent"
            )
            .is_err()
        );

        // The person moves and hides; hidden subtrees are dropped, undo brings them back.
        let m = s.layout(&a.id, "move", "d", -1).unwrap();
        assert_eq!(m.components[0]["children"], json!(["a", "d", "b"]));
        assert!(s.layout(&a.id, "move", "a", -1).is_err());
        assert!(s.layout(&a.id, "hide", "root", 1).is_err());
        let h = s.layout(&a.id, "hide", "b", 1).unwrap();
        assert!(!h.components.iter().any(|c| c["id"] == "b1"));
        assert_eq!(s.versions(&a.id).last().unwrap().by, "user");
        let u = s.restore(&a.id, None).unwrap();
        assert!(u.components.iter().any(|c| c["id"] == "b1"));
        assert_eq!(u.sources.len(), 2);
    }

    #[test]
    fn live_sources() {
        let s = store();
        let spec = json!({"components": [{"id": "root", "component": "Text"}],
            "sources": [{"id": "cpu", "command": "echo 5", "target": "/cpu", "parse": "number", "keep": 2}],
            "alerts": [{"when": "/cpu/0/value > 1", "text": "hot"}, {"text": "no condition"}]});
        assert!(validate(&spec).unwrap_err().to_string().contains("needs 'when'"));
        let spec = json!({"components": spec["components"], "sources": spec["sources"], "alerts": [spec["alerts"][0]]});
        let a = s.render(None, "t", &spec, "agent").unwrap();
        assert_eq!(a.alerts[0]["id"], "alert0");
        let src = a.sources[0].clone();
        for v in [1, 2, 3] {
            s.write_live(&a.id, &src, json!(v), "12:00:00").unwrap();
        }
        let got = s.get(&a.id).unwrap();
        assert_eq!(got.data["cpu"].as_array().unwrap().len(), 2);
        assert_eq!(got.data["cpu"][1]["value"], 3);
        assert_eq!(got.data["cpu"][1]["t"], "12:00:00");
        assert_eq!(got.version, 1, "live values are not versions");

        assert!(got.approved.is_empty());
        let ok = s.approve(&a.id, &["cpu".into()]).unwrap();
        assert_eq!(ok.approved, vec![source_key(&src)]);
    }

    #[test]
    fn parses_output() {
        let sh = |body: &str| format!("Exit code: 0\nOutput:\n{body}");
        assert_eq!(parse_output("shell", "number", &sh("load: 0,75 1.2")).unwrap(), json!(0.75));
        assert_eq!(parse_output("shell", "number", &sh("-3 C")).unwrap(), json!(-3.0));
        assert_eq!(parse_output("shell", "lines", &sh("a\n\nb\n")).unwrap(), json!(["a", "b"]));
        assert_eq!(parse_output("shell", "json", &sh("{\"a\": [1]}")).unwrap(), json!({"a": [1]}));
        assert_eq!(
            parse_output("shell", "table", &sh("PID CPU COMMAND\n1 0.5 init --x\n22 12 my app")).unwrap(),
            json!([{"PID": 1, "CPU": 0.5, "COMMAND": "init --x"}, {"PID": 22, "CPU": 12, "COMMAND": "my app"}])
        );
        assert_eq!(
            parse_output("shell", "kv", &sh("MemTotal: 100 kB\nname=x\nnoise")).unwrap(),
            json!({"MemTotal": "100 kB", "name": "x"})
        );
        assert_eq!(parse_output("shell", "text", &sh("hi\n")).unwrap(), json!("hi"));
        assert!(
            parse_output("shell", "text", "Exit code: 2\nOutput:\nboom")
                .unwrap_err()
                .to_string()
                .contains("exit code 2")
        );
        assert!(parse_output("shell", "number", &sh("none")).is_err());
        assert!(parse_output("read_file", "text", "error: no such file").is_err());
        assert_eq!(parse_output("read_file", "lines", "x\ny").unwrap(), json!(["x", "y"]));
    }
}
