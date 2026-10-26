//! Threads: append-only JSONL files of records with unique ids, so copies
//! from different devices merge by union.

use crate::types::{Message, new_id, now_ms};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub ts: i64,
    #[serde(flatten)]
    pub msg: Message,
}

impl Record {
    pub fn new(msg: Message) -> Self {
        Record { id: new_id(), ts: now_ms(), msg }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadInfo {
    pub name: String,
    pub updated: i64,
    pub messages: usize,
    pub title: String,
    /// The thread this one was branched from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
}

#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
}

pub fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 100
        || !name.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c))
        || name.starts_with('.')
    {
        bail!("invalid thread name '{name}' (letters, digits, - _ . only)");
    }
    Ok(())
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).ok();
        Store { dir }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.jsonl"))
    }

    pub fn load(&self, name: &str) -> Result<Vec<Record>> {
        valid_name(name)?;
        let Ok(text) = std::fs::read_to_string(self.file(name)) else { return Ok(vec![]) };
        Ok(text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
    }

    pub fn messages(&self, name: &str) -> Result<Vec<Message>> {
        Ok(self.load(name)?.into_iter().map(|r| r.msg).collect())
    }

    pub fn append(&self, name: &str, msg: &Message) -> Result<Record> {
        valid_name(name)?;
        let rec = Record::new(msg.clone());
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.file(name))?;
        writeln!(f, "{}", serde_json::to_string(&rec)?)?;
        Ok(rec)
    }

    /// Union with records from elsewhere. Returns how many were new.
    pub fn merge(&self, name: &str, incoming: Vec<Record>) -> Result<usize> {
        let mut all = self.load(name)?;
        let known: std::collections::HashSet<String> = all.iter().map(|r| r.id.clone()).collect();
        let fresh: Vec<Record> = incoming.into_iter().filter(|r| !known.contains(&r.id)).collect();
        let n = fresh.len();
        if n == 0 {
            return Ok(0);
        }
        all.extend(fresh);
        all.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.id.cmp(&b.id)));
        let text: String = all.iter().map(|r| serde_json::to_string(r).unwrap() + "\n").collect();
        let tmp = self.file(name).with_extension("jsonl.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, self.file(name))?;
        Ok(n)
    }

    pub fn list(&self) -> Vec<ThreadInfo> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let Ok(recs) = self.load(&name) else { continue };
            let meta = self.meta(&name);
            let title = meta["title"]
                .as_str()
                .filter(|t| !t.is_empty())
                .map(String::from)
                .or_else(|| {
                    recs.iter().find_map(|r| match &r.msg {
                        Message::User { content } => Some(crate::types::preview(content, 80)),
                        _ => None,
                    })
                })
                .unwrap_or_default();
            out.push(ThreadInfo {
                updated: recs.last().map(|r| r.ts).unwrap_or(0),
                messages: recs.len(),
                forked_from: meta["forked_from"].as_str().map(String::from),
                name,
                title,
            });
        }
        out.sort_by_key(|t| std::cmp::Reverse(t.updated));
        out
    }

    /// Threads whose messages contain `q` (case-insensitive), with a snippet around the match.
    pub fn search(&self, q: &str) -> Vec<(String, String)> {
        let q = q.to_lowercase();
        let mut out = Vec::new();
        for t in self.list() {
            let found = self.load(&t.name).unwrap_or_default().into_iter().find_map(|r| {
                let text = match r.msg {
                    Message::User { content } => content,
                    Message::Assistant { text: Some(t), .. } => t,
                    _ => return None,
                };
                let lower = text.to_lowercase();
                let at = lower.find(&q)?;
                // Byte offsets of the lowercased text may differ; count chars instead.
                let start = lower[..at].chars().count().saturating_sub(40);
                let snippet: String = text.chars().skip(start).take(120).collect();
                Some(snippet.split_whitespace().collect::<Vec<_>>().join(" "))
            });
            if let Some(s) = found.or_else(|| t.title.to_lowercase().contains(&q).then(String::new)) {
                out.push((t.name, s));
            }
        }
        out
    }

    /// A new thread with the first `upto` records of this one.
    pub fn fork(&self, name: &str, upto: usize) -> Result<String> {
        let recs = self.load(name)?;
        let new = new_thread_name();
        let text: String = recs.iter().take(upto).map(|r| serde_json::to_string(r).unwrap() + "\n").collect();
        std::fs::write(self.file(&new), text)?;
        let mut meta = self.meta(name);
        meta["forked_from"] = Value::String(name.to_string());
        meta.as_object_mut().map(|m| m.remove("web"));
        std::fs::write(self.dir.join(format!("{new}.meta.json")), serde_json::to_string_pretty(&meta)?)?;
        Ok(new)
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        valid_name(name)?;
        std::fs::remove_file(self.file(name))?;
        std::fs::remove_file(self.dir.join(format!("{name}.meta.json"))).ok();
        Ok(())
    }

    /// Small per-thread key/value data (e.g. web chat URLs).
    pub fn meta(&self, name: &str) -> Value {
        std::fs::read_to_string(self.dir.join(format!("{name}.meta.json")))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(serde_json::json!({}))
    }

    pub fn set_meta(&self, name: &str, key: &str, value: Value) -> Result<()> {
        valid_name(name)?;
        let mut m = self.meta(name);
        m[key] = value;
        std::fs::write(self.dir.join(format!("{name}.meta.json")), serde_json::to_string_pretty(&m)?)?;
        Ok(())
    }
}

pub fn new_thread_name() -> String {
    format!("{}-{:04x}", chrono::Local::now().format("%Y%m%d-%H%M"), rand::random::<u16>())
}

/// Make history acceptable to every API: each tool call gets a result.
pub fn repair(messages: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    let mut pending: Vec<(String, String)> = Vec::new();
    let flush = |out: &mut Vec<Message>, pending: &mut Vec<(String, String)>| {
        for (id, name) in pending.drain(..) {
            out.push(Message::Tool { call_id: id, name, content: "(interrupted: no result)".into() });
        }
    };
    for m in messages {
        match &m {
            Message::Tool { call_id, .. } => {
                if let Some(i) = pending.iter().position(|(id, _)| id == call_id) {
                    pending.remove(i);
                    out.push(m);
                }
                continue;
            }
            _ => flush(&mut out, &mut pending),
        }
        if let Message::Assistant { tool_calls, .. } = &m {
            pending = tool_calls.iter().map(|t| (t.id.clone(), t.name.clone())).collect();
        }
        out.push(m);
    }
    flush(&mut out, &mut pending);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolCall;

    #[test]
    fn merge_is_union() {
        let a = Store::new(std::env::temp_dir().join(format!("kron-a-{}", new_id())));
        let b = Store::new(std::env::temp_dir().join(format!("kron-b-{}", new_id())));
        a.append("t", &Message::user("one")).unwrap();
        b.merge("t", a.load("t").unwrap()).unwrap();
        a.append("t", &Message::user("from a")).unwrap();
        b.append("t", &Message::user("from b")).unwrap();
        assert_eq!(b.merge("t", a.load("t").unwrap()).unwrap(), 1);
        assert_eq!(a.merge("t", b.load("t").unwrap()).unwrap(), 1);
        let ids = |s: &Store| s.load("t").unwrap().into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(&a), ids(&b));
        assert_eq!(ids(&a).len(), 3);
    }

    #[test]
    fn fork_and_search() {
        let s = Store::new(std::env::temp_dir().join(format!("kron-f-{}", new_id())));
        s.append("t", &Message::user("Привет, Мир")).unwrap();
        s.append("t", &Message::assistant("answer")).unwrap();
        s.set_meta("t", "title", serde_json::json!("named")).unwrap();
        let f = s.fork("t", 1).unwrap();
        assert_eq!(s.load(&f).unwrap().len(), 1);
        assert_eq!(s.meta(&f)["forked_from"], "t");
        let hits = s.search("мир");
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|(_, snip)| snip.contains("Мир")));
        assert!(s.list().iter().any(|t| t.title == "named"));
    }

    #[test]
    fn repair_fills_missing_results() {
        let call =
            |id: &str| ToolCall { id: id.into(), name: "shell".into(), arguments: serde_json::json!({}), meta: None };
        let msgs = vec![
            Message::user("x"),
            Message::Assistant { text: None, tool_calls: vec![call("1"), call("2")], reasoning: None },
            Message::Tool { call_id: "1".into(), name: "shell".into(), content: "ok".into() },
            Message::user("y"),
            Message::Tool { call_id: "zzz".into(), name: "shell".into(), content: "orphan".into() },
        ];
        let r = repair(msgs);
        assert_eq!(r.len(), 5);
        assert!(matches!(&r[3], Message::Tool { call_id, .. } if call_id == "2"));
    }

    #[test]
    fn names() {
        assert!(valid_name("../x").is_err());
        assert!(valid_name("main-1.b").is_ok());
    }
}
