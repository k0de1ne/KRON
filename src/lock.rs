//! Locks: calls to the hub that go through only after the person confirms with
//! their password. The agent and the interface it writes may add a lock, but
//! cannot take one off or learn the password: removing a lock and changing the
//! password are locked themselves, and the confirmation page is kron's own
//! (`/kron/confirm`: it cannot be framed or scripted by the interface).
//!
//! A confirmation lets through exactly the request the person saw (method,
//! path and body), once, within two minutes.
//!
//! Lost the password: delete `~/.kron/locks.json` on the hub (this also takes
//! every lock off; it needs the machine, not the interface).

use crate::config::Config;
use crate::types::{new_id, now_ms};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Mutex;

const CONFIRM_TTL_MS: i64 = 120_000;
const ROUNDS: u32 = 100_000;
const METHODS: &[&str] = &["*", "GET", "POST", "PUT", "PATCH", "DELETE"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lock {
    pub id: String,
    pub title: String,
    /// GET, POST, ... or * for any.
    pub method: String,
    /// Path prefix, e.g. /api/ui/data/vault or /api/config.
    pub path: String,
    pub created: i64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    salt: String,
    hash: String,
    set: i64,
    locks: Vec<Lock>,
}

/// A request waiting for the person.
#[derive(Debug, Clone, Serialize)]
pub struct Challenge {
    pub id: String,
    pub title: String,
    pub method: String,
    pub path: String,
    /// The start of the body, to show what exactly is being confirmed.
    pub body: String,
    #[serde(skip)]
    hash: String,
    pub created: i64,
    pub approved: bool,
}

static CHALLENGES: Mutex<Vec<Challenge>> = Mutex::new(Vec::new());
/// Failed password attempts: (count, time of the last one).
static FAILS: Mutex<(u32, i64)> = Mutex::new((0, 0));
static FILE: Mutex<()> = Mutex::new(());

#[derive(Clone)]
pub struct Locks {
    file: PathBuf,
}

impl Locks {
    pub fn new(cfg: &Config) -> Self {
        Self::at(cfg.path("locks.json"))
    }

    pub fn at(file: PathBuf) -> Self {
        Locks { file }
    }

    fn load(&self) -> State {
        std::fs::read_to_string(&self.file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, s: &State) -> Result<()> {
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(s)?)?;
        std::fs::rename(tmp, &self.file)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<Lock> {
        self.load().locks
    }

    pub fn has_password(&self) -> bool {
        !self.load().hash.is_empty()
    }

    /// Anyone may add a lock: it only makes things stricter.
    pub fn add(&self, title: &str, method: &str, path: &str) -> Result<Lock> {
        let _g = FILE.lock().unwrap();
        let method = if method.is_empty() { "*".to_string() } else { method.to_uppercase() };
        if !METHODS.contains(&method.as_str()) {
            bail!("method must be one of {}", METHODS.join(", "));
        }
        if !path.starts_with('/') || path.starts_with("/kron") || path.starts_with("/api/presence") || path.len() > 300
        {
            bail!("path must start with / (not /kron or /api/presence)");
        }
        let mut s = self.load();
        if let Some(l) = s.locks.iter().find(|l| l.method == method && l.path == path) {
            return Ok(l.clone());
        }
        let lock = Lock {
            id: format!("l-{}", new_id()),
            title: title.trim().chars().take(120).collect(),
            method,
            path: path.to_string(),
            created: now_ms(),
        };
        s.locks.push(lock.clone());
        self.save(&s)?;
        Ok(lock)
    }

    /// Only after a confirmation (removing is itself locked).
    pub fn remove(&self, id: &str) -> Result<()> {
        let _g = FILE.lock().unwrap();
        let mut s = self.load();
        let before = s.locks.len();
        s.locks.retain(|l| l.id != id);
        if s.locks.len() == before {
            bail!("no lock '{id}'");
        }
        self.save(&s)
    }

    /// The lock a call falls under, if any. Taking locks off and changing the
    /// password are always locked once there is a password.
    pub fn find(&self, method: &str, path: &str) -> Option<Lock> {
        let s = self.load();
        let builtin = |title: &str| Lock {
            id: "kernel".into(),
            title: title.into(),
            method: method.into(),
            path: path.into(),
            created: 0,
        };
        if method == "DELETE" && path.starts_with("/api/locks/") {
            return Some(builtin("Снять замок"));
        }
        if path == "/api/locks/password" && !s.hash.is_empty() {
            return Some(builtin("Сменить пароль"));
        }
        s.locks.into_iter().find(|l| {
            (l.method == "*" || l.method == method)
                && (path == l.path
                    || l.path.ends_with('/') && path.starts_with(&l.path)
                    || path.starts_with(&format!("{}/", l.path))
                    || path.starts_with(&format!("{}?", l.path)))
        })
    }

    /// A locked request waits for the person on the returned challenge; None when
    /// they already confirmed exactly this request (it goes through, once).
    pub fn wait(&self, lock: &Lock, method: &str, path: &str, body: &[u8]) -> Option<Challenge> {
        let hash = request_hash(method, path, body);
        let mut all = CHALLENGES.lock().unwrap();
        all.retain(|c| now_ms() - c.created < CONFIRM_TTL_MS);
        if let Some(i) = all.iter().position(|c| c.approved && c.hash == hash) {
            all.remove(i);
            return None;
        }
        if let Some(c) = all.iter().find(|c| !c.approved && c.hash == hash) {
            return Some(c.clone());
        }
        let c = Challenge {
            id: new_id(),
            title: lock.title.clone(),
            method: method.into(),
            path: path.into(),
            body: crate::types::preview(&String::from_utf8_lossy(body), 600),
            hash,
            created: now_ms(),
            approved: false,
        };
        all.push(c.clone());
        if all.len() > 50 {
            all.remove(0);
        }
        Some(c)
    }

    pub fn challenge(&self, id: &str) -> Option<Challenge> {
        CHALLENGES.lock().unwrap().iter().find(|c| c.id == id && now_ms() - c.created < CONFIRM_TTL_MS).cloned()
    }

    /// The person confirms a challenge with their password; the first time, the password is set.
    pub fn confirm(&self, id: &str, password: &str) -> Result<()> {
        {
            let f = FAILS.lock().unwrap();
            if f.0 >= 5 && now_ms() - f.1 < 60_000 {
                bail!("too many wrong passwords, wait a minute");
            }
        }
        if self.challenge(id).is_none() {
            bail!("this confirmation has expired; repeat the action");
        }
        let _g = FILE.lock().unwrap();
        let mut s = self.load();
        if s.hash.is_empty() {
            if password.chars().count() < 6 {
                bail!("the password needs at least 6 characters");
            }
            s.salt = format!("{:032x}", rand::random::<u128>());
            s.hash = stretch(&s.salt, password);
            s.set = now_ms();
            self.save(&s)?;
        } else if stretch(&s.salt, password) != s.hash {
            let mut f = FAILS.lock().unwrap();
            *f = (if now_ms() - f.1 < 60_000 { f.0 + 1 } else { 1 }, now_ms());
            bail!("wrong password");
        }
        *FAILS.lock().unwrap() = (0, 0);
        if let Some(c) = CHALLENGES.lock().unwrap().iter_mut().find(|c| c.id == id) {
            c.approved = true;
            c.created = now_ms();
        }
        Ok(())
    }

    /// After a confirmed "change password" call: the new one.
    pub fn set_password(&self, password: &str) -> Result<()> {
        if password.chars().count() < 6 {
            bail!("the password needs at least 6 characters");
        }
        let _g = FILE.lock().unwrap();
        let mut s = self.load();
        s.salt = format!("{:032x}", rand::random::<u128>());
        s.hash = stretch(&s.salt, password);
        s.set = now_ms();
        self.save(&s)
    }
}

fn request_hash(method: &str, path: &str, body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(method.as_bytes());
    h.update(b" ");
    h.update(path.as_bytes());
    h.update(b"\n");
    h.update(body);
    format!("{:x}", h.finalize())
}

fn stretch(salt: &str, password: &str) -> String {
    let mut h: [u8; 32] = Sha256::digest(format!("{salt}:{password}").as_bytes()).into();
    for _ in 0..ROUNDS {
        let mut d = Sha256::new();
        d.update(h);
        d.update(salt.as_bytes());
        h = d.finalize().into();
    }
    h.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_and_confirmations() {
        let l = Locks::at(std::env::temp_dir().join(format!("kron-lock-{}.json", new_id())));
        assert!(l.find("GET", "/api/ui/data/vault").is_none());
        let lock = l.add("Пароли", "get", "/api/ui/data/vault").unwrap();
        assert!(l.add("x", "GET", "/kron/confirm").is_err());
        assert!(l.add("x", "FETCH", "/a").is_err());
        assert_eq!(l.add("again", "GET", "/api/ui/data/vault").unwrap().id, lock.id);
        assert!(l.find("GET", "/api/ui/data/vault").is_some());
        assert!(l.find("GET", "/api/ui/data/vault?x=1").is_some());
        assert!(l.find("GET", "/api/ui/data/vaults").is_none());
        assert!(l.find("PUT", "/api/ui/data/vault").is_none());
        assert!(l.find("DELETE", &format!("/api/locks/{}", lock.id)).is_some(), "removing is always locked");
        assert!(l.find("POST", "/api/locks/password").is_none(), "no password yet");

        let c = l.wait(&lock, "GET", "/api/ui/data/vault", b"").unwrap();
        // The same request waits on the same challenge.
        assert_eq!(l.wait(&lock, "GET", "/api/ui/data/vault", b"").unwrap().id, c.id);
        assert!(l.confirm(&c.id, "123").is_err(), "too short for a first password");
        l.confirm(&c.id, "secret-1").unwrap();
        assert!(l.has_password());
        // Exactly that request goes through, once.
        assert!(l.wait(&lock, "GET", "/api/ui/data/vault?other", b"").is_some());
        assert!(l.wait(&lock, "GET", "/api/ui/data/vault", b"").is_none());
        let c2 = l.wait(&lock, "GET", "/api/ui/data/vault", b"").unwrap();
        assert!(l.confirm(&c2.id, "wrong").is_err());
        l.confirm(&c2.id, "secret-1").unwrap();
        assert!(l.find("POST", "/api/locks/password").is_some());
        l.remove(&lock.id).unwrap();
        assert!(l.list().is_empty());
        assert!(l.confirm("nope", "secret-1").is_err());
    }
}
