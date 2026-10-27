//! What kron learns about the person: explicit notes, which components they
//! actually use for which kind of task, recent tasks, and layouts that worked
//! (templates) to build from next time instead of designing from scratch.
//!
//! Stored on the hub (`~/.kron/genui/profile.json`, `templates/`), so it does
//! not depend on a chat, a model or a device.

use super::surface::Surface;
use crate::types::{new_id, now_ms};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_NOTES: usize = 40;
const MAX_RECENT: usize = 15;
const TEMPLATES_PER_INTENT: usize = 3;
const MAX_TEMPLATES: usize = 40;

/// Kinds of tasks. The agent may name others; these are what the
/// keyword classifier knows.
pub const INTENTS: &[(&str, &str)] = &[
    ("compare", "comparison or analysis of options: dashboard with metrics, charts, filters"),
    ("project", "understanding a codebase or project: structure, dependencies, tasks, actions"),
    ("server", "setting up or operating a machine or service: status, controls, terminal, logs"),
    ("code", "writing or changing code: editor, diff, tests, agent controls"),
    ("data", "looking at a dataset or file: table, chart, filters"),
    ("plan", "planning or tracking: checklist, timeline, progress"),
    ("write", "writing text: editor with the draft and variants"),
    ("chat", "just talking or a short question: plain text, no interface"),
];

/// Lower-case stems that point to an intent (Russian and English).
const KEYWORDS: &[(&str, &[&str])] = &[
    (
        "compare",
        &[
            "сравн",
            "срав",
            "versus",
            " vs",
            "vs.",
            "compar",
            "лучше",
            "отлич",
            "разниц",
            "выбра",
            "выбор",
            "аналит",
            "бенчмар",
            "benchmark",
            "рейтинг",
            "рынок",
            "конкурент",
            "плюсы",
            "минусы",
            "pros",
        ],
    ),
    (
        "project",
        &[
            "проект",
            "репозит",
            "codebase",
            "кодоваябаз",
            "кодовой баз",
            "структур",
            "архитект",
            "разберись",
            "зависимост",
            "dependenc",
            "repo",
            "project",
            "модул",
        ],
    ),
    (
        "server",
        &[
            "сервер",
            "server",
            "nginx",
            "docker",
            "деплой",
            "deploy",
            "systemd",
            "настрой",
            "запусти сервис",
            "лог",
            "logs",
            "порт",
            "ssh",
            "диск",
            "памят",
            "процесс",
            "cpu",
            "машин",
            "kubernetes",
            "k8s",
            "мониторинг",
            "uptime",
        ],
    ),
    (
        "code",
        &[
            "код",
            "code",
            "функци",
            "function",
            "напиши программ",
            "скрипт",
            "script",
            "рефактор",
            "refactor",
            "баг",
            "bug",
            "тест",
            "test",
            "почини",
            "fix",
            "реализ",
            "implement",
            "класс",
            "diff",
        ],
    ),
    (
        "data",
        &[
            "таблиц",
            "csv",
            "excel",
            "xlsx",
            "данны",
            "dataset",
            "data",
            "график",
            "chart",
            "статистик",
            "json",
            "sql",
            "выборк",
        ],
    ),
    (
        "plan",
        &[
            "план",
            "plan",
            "задач",
            "todo",
            "список дел",
            "чеклист",
            "checklist",
            "трекер",
            "расписан",
            "schedule",
            "дедлайн",
            "roadmap",
            "этап",
        ],
    ),
    (
        "write",
        &[
            "напиши текст",
            "письм",
            "email",
            "статью",
            "статья",
            "пост",
            "текст",
            "letter",
            "essay",
            "перевед",
            "translat",
            "резюме",
            "отредакт",
        ],
    ),
    (
        "chat",
        &[
            "поговорим",
            "поболтаем",
            "привет",
            "как дела",
            "hello",
            "hi ",
            "кто ты",
            "что думаешь",
            "расскажи",
            "объясни",
            "explain",
            "почему",
            "why",
            "просто",
        ],
    ),
];

/// The most likely intent of a request, with a 0..1 confidence.
pub fn classify(text: &str) -> (String, f32) {
    let t = format!(" {} ", text.to_lowercase());
    let mut best = ("general", 0usize);
    let mut total = 0usize;
    for (intent, words) in KEYWORDS {
        let hits = words.iter().filter(|w| t.contains(*w)).count();
        total += hits;
        if hits > best.1 {
            best = (intent, hits);
        }
    }
    if best.1 == 0 {
        // Short text without a task in it is conversation.
        let words = t.split_whitespace().count();
        return if words <= 4 { ("chat".into(), 0.4) } else { ("general".into(), 0.0) };
    }
    (best.0.to_string(), best.1 as f32 / total as f32)
}

const STOP: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "что",
    "как",
    "это",
    "мне",
    "мой",
    "моё",
    "мою",
    "моем",
    "моём",
    "мои",
    "для",
    "или",
    "все",
    "всё",
    "его",
    "она",
    "они",
    "так",
    "там",
    "тут",
    "вот",
    "надо",
    "нужно",
    "пожалуйста",
    "please",
    "can",
    "you",
    "your",
    "this",
    "that",
    "сделай",
    "покажи",
    "make",
    "show",
];

/// Words of a request, cut to a short stem so "проекте" and "проект" match.
pub fn keywords(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(w))
        .map(|w| w.chars().take(6).collect::<String>())
        .filter(|w| seen.insert(w.clone()))
        .take(30)
        .collect()
}

pub fn similarity(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (sa, sb): (HashSet<&String>, HashSet<&String>) = (a.iter().collect(), b.iter().collect());
    let inter = sa.intersection(&sb).count() as f32;
    inter / (sa.len().min(sb.len()) as f32)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Things the person said about how they like to work ("show prices in rubles").
    pub notes: Vec<Note>,
    /// intent → component → weight (shown: +0.2; used: +1; on a liked surface: +2; disliked: -2).
    pub affinity: BTreeMap<String, BTreeMap<String, f32>>,
    /// intent → requests seen.
    pub counts: BTreeMap<String, u64>,
    pub recent: Vec<Recent>,
    pub updated: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Note {
    pub id: String,
    pub text: String,
    pub ts: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Recent {
    pub title: String,
    pub intent: String,
    pub surface: String,
    pub ts: i64,
}

/// A layout that worked: the components of a surface the person liked, used a
/// lot or pinned. Its data is kept as an example of the shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Template {
    pub id: String,
    pub intent: String,
    pub title: String,
    pub keywords: Vec<String>,
    pub root: String,
    pub components: Vec<Value>,
    pub data: Value,
    pub source: String,
    pub score: f32,
    pub uses: u64,
    pub updated: i64,
}

static LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone)]
pub struct Memory {
    dir: PathBuf,
}

impl Memory {
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(dir.join("templates")).ok();
        Memory { dir }
    }

    pub fn profile(&self) -> Profile {
        std::fs::read_to_string(self.dir.join("profile.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, p: &Profile) -> Result<()> {
        let f = self.dir.join("profile.json");
        let tmp = f.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(p)?)?;
        std::fs::rename(tmp, f)?;
        Ok(())
    }

    fn change(&self, f: impl FnOnce(&mut Profile)) -> Result<Profile> {
        let _g = LOCK.lock().unwrap();
        let mut p = self.profile();
        f(&mut p);
        p.updated = now_ms();
        self.save(&p)?;
        Ok(p)
    }

    pub fn remember(&self, text: &str) -> Result<Note> {
        let text: String = text.trim().chars().take(300).collect();
        let note = Note { id: new_id(), text: text.clone(), ts: now_ms() };
        let n2 = note.clone();
        self.change(move |p| {
            p.notes.retain(|n| n.text.to_lowercase() != text.to_lowercase());
            p.notes.push(n2);
            if p.notes.len() > MAX_NOTES {
                p.notes.remove(0);
            }
        })?;
        Ok(note)
    }

    pub fn forget(&self, id: &str) -> Result<()> {
        self.change(|p| p.notes.retain(|n| n.id != id)).map(|_| ())
    }

    /// Forget everything learned (notes stay unless `notes`).
    pub fn reset(&self, notes: bool) -> Result<()> {
        self.change(|p| {
            p.affinity.clear();
            p.counts.clear();
            p.recent.clear();
            if notes {
                p.notes.clear();
            }
        })?;
        for t in self.templates() {
            self.delete_template(&t.id).ok();
        }
        Ok(())
    }

    /// A surface was rendered for a request.
    pub fn rendered(&self, s: &Surface, first: bool) -> Result<()> {
        let kinds = component_kinds(&s.components);
        self.change(|p| {
            let a = p.affinity.entry(s.intent.clone()).or_default();
            for k in &kinds {
                *a.entry(k.clone()).or_default() += 0.2;
            }
            if first {
                *p.counts.entry(s.intent.clone()).or_default() += 1;
            }
            p.recent.retain(|r| r.surface != s.id);
            p.recent.push(Recent {
                title: s.title.clone(),
                intent: s.intent.clone(),
                surface: s.id.clone(),
                ts: now_ms(),
            });
            if p.recent.len() > MAX_RECENT {
                p.recent.remove(0);
            }
        })?;
        Ok(())
    }

    /// The person used components of the surface (`used`: component types),
    /// liked or disliked it. Good surfaces become templates; a disliked one
    /// stops being one.
    pub fn signal(&self, s: &Surface, used: &[String], kind: &str) -> Result<()> {
        let all = component_kinds(&s.components);
        self.change(|p| {
            let a = p.affinity.entry(s.intent.clone()).or_default();
            for k in used {
                *a.entry(k.clone()).or_default() += 1.0;
            }
            let w = match kind {
                "like" | "pin" => 2.0,
                "dislike" => -2.0,
                _ => 0.0,
            };
            if w != 0.0 {
                for k in &all {
                    *a.entry(k.clone()).or_default() += w;
                }
            }
        })?;
        let good = s.pinned || s.stats.likes > s.stats.dislikes || (s.stats.interactions >= 3 && s.stats.dislikes == 0);
        if kind == "dislike" {
            self.demote(&s.id)?;
        } else if good && !s.intent.is_empty() && s.intent != "chat" {
            self.promote(s)?;
        }
        Ok(())
    }

    pub fn templates(&self) -> Vec<Template> {
        let mut out: Vec<Template> = std::fs::read_dir(self.dir.join("templates"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score).then(b.updated.cmp(&a.updated)));
        out
    }

    fn template_file(&self, id: &str) -> PathBuf {
        self.dir.join("templates").join(format!("{id}.json"))
    }

    pub fn delete_template(&self, id: &str) -> Result<()> {
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            anyhow::bail!("bad template id");
        }
        std::fs::remove_file(self.template_file(id))?;
        Ok(())
    }

    fn put_template(&self, t: &Template) -> Result<()> {
        std::fs::write(self.template_file(&t.id), serde_json::to_string(t)?)?;
        Ok(())
    }

    /// Keep this surface's layout as a template for its intent.
    pub fn promote(&self, s: &Surface) -> Result<Template> {
        let _g = LOCK.lock().unwrap();
        let all = self.templates();
        let score =
            1.0 + s.stats.interactions as f32 * 0.3 + s.stats.likes as f32 * 2.0 + if s.pinned { 3.0 } else { 0.0 }
                - s.stats.dislikes as f32 * 2.0;
        let mut t = all
            .iter()
            .find(|t| t.source == s.id)
            .cloned()
            .unwrap_or_else(|| Template { id: format!("t-{}", new_id()), ..Default::default() });
        t.intent = s.intent.clone();
        t.title = s.title.clone();
        t.keywords = keywords(&format!("{} {}", s.title, s.request));
        t.root = s.root.clone();
        t.components = s.components.clone();
        t.data = s.data.clone();
        t.source = s.id.clone();
        t.score = score;
        t.updated = now_ms();
        self.put_template(&t)?;
        // Only the best few per intent, and a cap overall.
        let mut same: Vec<Template> = self.templates().into_iter().filter(|x| x.intent == t.intent).collect();
        same.sort_by(|a, b| b.score.total_cmp(&a.score));
        for x in same.iter().skip(TEMPLATES_PER_INTENT) {
            std::fs::remove_file(self.template_file(&x.id)).ok();
        }
        for x in self.templates().iter().skip(MAX_TEMPLATES) {
            std::fs::remove_file(self.template_file(&x.id)).ok();
        }
        Ok(t)
    }

    fn demote(&self, source: &str) -> Result<()> {
        let _g = LOCK.lock().unwrap();
        for mut t in self.templates().into_iter().filter(|t| t.source == source) {
            t.score -= 3.0;
            if t.score <= 0.0 {
                std::fs::remove_file(self.template_file(&t.id)).ok();
            } else {
                self.put_template(&t)?;
            }
        }
        Ok(())
    }

    /// The layout to start from for this request, if one fits well enough.
    pub fn best_template(&self, intent: &str, text: &str) -> Option<(Template, f32)> {
        let kw = keywords(text);
        self.templates()
            .into_iter()
            .filter(|t| t.intent == intent)
            .map(|t| {
                let sim = similarity(&kw, &t.keywords);
                let fit = 0.4 + 0.6 * sim + (t.score / 20.0).min(0.2);
                (t, fit)
            })
            .filter(|(_, fit)| *fit >= 0.45)
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    pub fn used_template(&self, id: &str) {
        let _g = LOCK.lock().unwrap();
        if let Ok(text) = std::fs::read_to_string(self.template_file(id))
            && let Ok(mut t) = serde_json::from_str::<Template>(&text)
        {
            t.uses += 1;
            self.put_template(&t).ok();
        }
    }

    /// Text about the person for the system prompt.
    pub fn describe(&self, intent: &str) -> String {
        let p = self.profile();
        let mut s = String::new();
        if !p.notes.is_empty() {
            s.push_str("What the user told you about their preferences (follow these):\n");
            for n in &p.notes {
                s.push_str(&format!("- {}\n", n.text));
            }
        }
        if let Some(a) = p.affinity.get(intent).filter(|a| !a.is_empty()) {
            let mut v: Vec<(&String, &f32)> = a.iter().collect();
            v.sort_by(|x, y| y.1.total_cmp(x.1));
            let liked: Vec<String> =
                v.iter().filter(|(_, w)| **w >= 1.0).take(6).map(|(k, w)| format!("{k} ({w:.0})")).collect();
            let disliked: Vec<String> =
                v.iter().filter(|(_, w)| **w < 0.0).take(4).map(|(k, _)| k.to_string()).collect();
            if !liked.is_empty() {
                s.push_str(&format!(
                    "For '{intent}' tasks the user actively uses: {}. Prefer these.\n",
                    liked.join(", ")
                ));
            }
            if !disliked.is_empty() {
                s.push_str(&format!(
                    "For '{intent}' tasks the user disliked layouts built around: {}. Avoid them.\n",
                    disliked.join(", ")
                ));
            }
        }
        if !p.recent.is_empty() {
            s.push_str("Recent tasks (newest last): ");
            let r: Vec<String> =
                p.recent.iter().rev().take(8).rev().map(|r| format!("{} [{}]", r.title, r.intent)).collect();
            s.push_str(&r.join("; "));
            s.push('\n');
        }
        s
    }

    /// For the settings screen: everything kron knows, readable.
    pub fn export(&self) -> Value {
        let p = self.profile();
        let templates: Vec<Value> = self
            .templates()
            .iter()
            .map(|t| json!({"id": t.id, "intent": t.intent, "title": t.title, "score": t.score, "uses": t.uses, "updated": t.updated, "source": t.source, "components": t.components.len()}))
            .collect();
        json!({"notes": p.notes, "affinity": p.affinity, "counts": p.counts, "recent": p.recent, "templates": templates})
    }
}

pub fn component_kinds(components: &[Value]) -> Vec<String> {
    let mut seen = HashSet::new();
    components
        .iter()
        .filter_map(|c| c["component"].as_str())
        .filter(|k| !matches!(*k, "Column" | "Row" | "Grid" | "Divider" | "Heading" | "Text" | "Card"))
        .filter(|k| seen.insert(k.to_string()))
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        assert_eq!(classify("Сравни Claude и Codex").0, "compare");
        assert_eq!(classify("Разберись в моём проекте").0, "project");
        assert_eq!(classify("Настрой сервер nginx").0, "server");
        assert_eq!(classify("Помоги написать код").0, "code");
        assert_eq!(classify("Просто поговорим").0, "chat");
        assert_eq!(classify("hello").0, "chat");
    }

    #[test]
    fn stems_match() {
        let a = keywords("Разберись в моём проекте kron");
        let b = keywords("структура проекта kron");
        assert!(similarity(&a, &b) > 0.4, "{a:?} {b:?}");
    }

    #[test]
    fn learns_and_reuses() {
        let m = Memory::new(std::env::temp_dir().join(format!("kron-m-{}", new_id())));
        let mut s = Surface {
            id: "s-1".into(),
            title: "Claude vs Codex".into(),
            intent: "compare".into(),
            request: "Сравни Claude и Codex".into(),
            root: "root".into(),
            components: vec![
                json!({"id": "root", "component": "Column", "children": ["c"]}),
                json!({"id": "c", "component": "Chart"}),
            ],
            ..Default::default()
        };
        m.rendered(&s, true).unwrap();
        assert_eq!(m.profile().counts["compare"], 1);
        assert!(m.best_template("compare", "сравни gemini и claude").is_none());

        s.stats.likes = 1;
        m.signal(&s, &["Chart".into()], "like").unwrap();
        let (t, fit) = m.best_template("compare", "Сравни Gemini и Claude").unwrap();
        assert_eq!(t.source, "s-1");
        assert!(fit > 0.5);
        assert!(m.describe("compare").contains("Chart"));
        // Something else entirely gets no template.
        assert!(m.best_template("server", "настрой nginx").is_none());

        s.stats.dislikes = 3;
        m.signal(&s, &[], "dislike").unwrap();
        assert!(m.best_template("compare", "Сравни Gemini и Claude").is_none());

        let n = m.remember("Цены в рублях").unwrap();
        m.remember("цены в рублях").unwrap();
        assert_eq!(m.profile().notes.len(), 1);
        m.forget(&m.profile().notes[0].id).unwrap();
        assert!(m.profile().notes.is_empty());
        let _ = n;
    }
}
