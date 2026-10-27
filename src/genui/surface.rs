//! Surfaces: interfaces the agent builds for a task. The format follows A2UI
//! v0.9 (a flat list of components that point to their children by id, a
//! separate data model addressed by JSON Pointer, `{"path": ...}` bindings,
//! `{"event": ...}` actions) with kron's own component catalog.
//!
//! A surface lives in `~/.kron/genui/surfaces/<id>.json`; every change made by
//! the agent or the person is a version in `<id>.versions.jsonl`, so any of
//! them can be undone.

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
        self.commit(&mut s, by, spec["note"].as_str().unwrap_or(""))?;
        Ok(s)
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
    Ok(Checked { root, components: out })
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
}
