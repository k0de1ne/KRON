use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Provider used when nothing else is chosen.
    pub default: Option<String>,
    /// Provider whose native web search backs the `web_search` tool.
    /// Defaults to the first provider that has one (codex, claude, gemini, antigravity).
    pub search: Option<String>,
    pub providers: BTreeMap<String, ProviderCfg>,
    pub agents: BTreeMap<String, AgentCfg>,
    pub hub: HubCfg,
    pub browser: BrowserCfg,
    /// Web chats driven through the browser. Built-in: chatgpt, gemini.
    pub sites: BTreeMap<String, SiteCfg>,

    #[serde(skip)]
    pub home: PathBuf,
    /// The file this config was read from (and is saved to by the web UI).
    #[serde(skip)]
    pub file: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProviderCfg {
    /// openai | responses | anthropic | codex | claude | gemini | antigravity | local
    pub kind: String,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub max_tokens: Option<u32>,
    /// Extra HTTP headers sent with every request.
    pub headers: BTreeMap<String, String>,
    /// Codex: reasoning effort (minimal | low | medium | high).
    pub reasoning_effort: Option<String>,
    /// Gemini / Antigravity: Google Cloud project (otherwise discovered automatically).
    pub project: Option<String>,
    /// OAuth client for token refresh and `kron login` (Google kinds need these).
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    /// Use a credentials file other than the official CLI's default one.
    pub credentials: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgentCfg {
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Extra instructions appended to the system prompt.
    pub system: Option<String>,
    pub system_file: Option<String>,
    /// Allowed tools. Empty = all.
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HubCfg {
    /// Server side: address to listen on, e.g. "0.0.0.0:7700".
    pub listen: Option<String>,
    /// Shared secret between the hub, its nodes and the web UI.
    pub token: Option<String>,
    /// Client side: where the hub lives, e.g. "http://my-server:7700".
    pub url: Option<String>,
    /// Name of this machine as a node. Defaults to the hostname.
    pub node: Option<String>,
    /// Working directory for tools run by `kron node` / on the hub itself.
    pub workdir: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BrowserCfg {
    /// Chrome / Chromium / Edge executable. Auto-detected if empty.
    pub path: Option<String>,
    pub headless: Option<bool>,
    pub args: Vec<String>,
    /// Disable web chats on this machine.
    pub disabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SiteCfg {
    pub url: String,
    /// Prompt input (textarea or contenteditable).
    pub input: String,
    pub send: String,
    /// Every assistant reply on the page; the last one is the answer.
    pub response: String,
    /// Present while the site is still generating.
    pub stop: String,
}

impl Config {
    pub fn load(explicit: Option<&Path>) -> Result<Config> {
        let path = explicit.map(Path::to_path_buf).unwrap_or_else(|| home_dir().join("config.toml"));
        let text = if path.exists() {
            std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?
        } else {
            String::new()
        };
        Config::parse(&text, path)
    }

    pub fn parse(text: &str, file: PathBuf) -> Result<Config> {
        let mut cfg: Config = toml::from_str(text).with_context(|| format!("parsing {}", file.display()))?;
        cfg.home = home_dir();
        cfg.file = file;
        for (name, site) in builtin_sites() {
            cfg.sites.entry(name.to_string()).or_insert(site);
        }
        // Downloaded local models are providers without any config.
        for (name, stem) in crate::local::installed(&cfg.home) {
            cfg.providers.entry(name).or_insert(ProviderCfg {
                kind: "local".into(),
                model: stem,
                ..Default::default()
            });
        }
        Ok(cfg)
    }

    pub fn path(&self, sub: &str) -> PathBuf {
        self.home.join(sub)
    }

    pub fn provider(&self, name: &str) -> Result<&ProviderCfg> {
        self.providers.get(name).with_context(|| {
            let known: Vec<_> = self.providers.keys().cloned().collect();
            format!("unknown provider '{name}' (configured: {})", known.join(", "))
        })
    }

    pub fn default_provider(&self) -> Result<String> {
        if let Some(d) = &self.default {
            return Ok(d.clone());
        }
        self.providers
            .keys()
            .next()
            .cloned()
            .context("no providers configured; run `kron init` and edit ~/.kron/config.toml")
    }

    pub fn search_provider(&self) -> Option<String> {
        if self.search.is_some() {
            return self.search.clone();
        }
        self.providers
            .iter()
            .find(|(_, p)| matches!(p.kind.as_str(), "codex" | "claude" | "gemini" | "antigravity"))
            .map(|(n, _)| n.clone())
    }

    pub fn node_name(&self) -> String {
        self.hub.node.clone().unwrap_or_else(hostname)
    }
}

pub fn home_dir() -> PathBuf {
    if let Ok(h) = std::env::var("KRON_HOME") {
        return PathBuf::from(h);
    }
    user_home().join(".kron")
}

pub fn user_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn hostname() -> String {
    let clean =
        |s: String| Some(s.trim().to_lowercase().replace(' ', "-")).filter(|s| !s.is_empty() && s != "localhost");
    let android_model = || {
        let out = std::process::Command::new("getprop").arg("ro.product.model").output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    };
    std::env::var("COMPUTERNAME")
        .ok()
        .and_then(clean)
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().and_then(clean))
        .or_else(|| std::env::var("HOSTNAME").ok().and_then(clean))
        .or_else(|| if cfg!(target_os = "android") { android_model().and_then(clean) } else { None })
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

fn builtin_sites() -> Vec<(&'static str, SiteCfg)> {
    vec![
        (
            "chatgpt",
            SiteCfg {
                url: "https://chatgpt.com/".into(),
                input: "#prompt-textarea".into(),
                send: "[data-testid=\"send-button\"]".into(),
                response: "[data-message-author-role=\"assistant\"]".into(),
                stop: "[data-testid=\"stop-button\"]".into(),
            },
        ),
        (
            "gemini",
            SiteCfg {
                url: "https://gemini.google.com/app".into(),
                input: "rich-textarea [contenteditable=\"true\"]".into(),
                send: "button.send-button".into(),
                response: "model-response message-content".into(),
                stop: "button.send-button.stop, button[aria-label*=\"Stop\"]".into(),
            },
        ),
    ]
}

pub const EXAMPLE: &str = r#"# kron config. Lives in ~/.kron/config.toml (or $KRON_HOME/config.toml).

default = "codex"        # provider used when -p is not given
# search = "codex"       # whose native web search backs the web_search tool

# ── Subscriptions (unofficial adapters, reuse the official CLIs' logins) ──────
# Credentials are read from ~/.codex/auth.json, ~/.claude/.credentials.json,
# ~/.gemini/oauth_creds.json, or created with `kron login <provider>`.

[providers.codex]
kind = "codex"           # ChatGPT Plus/Pro via the Codex backend
model = "gpt-6-astra"

[providers.claude]
kind = "claude"          # Claude Pro/Max via Claude Code OAuth
model = "claude-opus-5-5"

[providers.gemini]
kind = "gemini"          # Gemini CLI login (Code Assist)
model = "gemini-3.8-flash"
# client_id / client_secret: the public Gemini CLI OAuth client, needed to
# refresh tokens and for `kron login gemini` (see README).

# [providers.antigravity]
# kind = "antigravity"   # Google Antigravity quota (Gemini 3, Claude)
# model = "gemini-3.8-flash"
# client_id = "..."
# client_secret = "..."

# ── Through CLIProxyAPI (github.com/router-for-me/CLIProxyAPI) ──────────────
# A maintained proxy for the same subscriptions. Run it next to the hub and
# point providers at it; native tools (web search) pass through.
# [providers.claude-cpa]
# kind = "anthropic"
# base_url = "http://127.0.0.1:8317"
# api_key = "<one of CLIProxyAPI's access.api-keys>"
# model = "claude-opus-5-5"
#
# [providers.codex-cpa]
# kind = "responses"
# base_url = "http://127.0.0.1:8317/v1"
# api_key = "<one of CLIProxyAPI's access.api-keys>"
# model = "gpt-6-astra"

# ── API keys ──────────────────────────────────────────────────────────────────

[providers.deepseek]
kind = "openai"
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
api_key_env = "DEEPSEEK_API_KEY"

# Same DeepSeek account through its Anthropic-compatible endpoint
# [providers.deepseek-a]
# kind = "anthropic"
# base_url = "https://api.deepseek.com/anthropic"
# model = "deepseek-chat"
# api_key_env = "DEEPSEEK_API_KEY"

# [providers.local]
# kind = "openai"
# base_url = "http://localhost:8080/v1"
# model = "gemma"

# ── Agents ────────────────────────────────────────────────────────────────────

[agents.orchestrator]
provider = "codex"
system = "Split big tasks and delegate parts to other agents with spawn_agent."

[agents.researcher]
provider = "gemini"
tools = ["web_search", "read_file", "list_dir", "grep_files"]
system = "Answer with sources. Prefer web_search for anything current."

# ── Sync: one hub, many nodes ────────────────────────────────────────────────

[hub]
# listen = "0.0.0.0:7700"            # on the machine that runs `kron serve`
# url = "http://my-server:7700"      # on every other machine
# token = "change-me"                # same everywhere
# node = "pc"                        # this machine's name

[browser]
# path = "C:/Program Files/Google/Chrome/Application/chrome.exe"
# headless = true
"#;
