//! OAuth credentials for subscription adapters.
//!
//! By default the official CLIs' credential files are used in place (and
//! refreshed tokens are written back in their format), so kron and the
//! official CLI share one login. `kron login <provider>` creates a separate
//! login stored in ~/.kron/auth/<provider>.json.

use crate::config::{user_home, Config, ProviderCfg};
use crate::llm::{check, http};
use crate::types::now_ms;
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Creds {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Unix milliseconds.
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// ChatGPT account id (codex).
    #[serde(default)]
    pub account_id: Option<String>,
    /// Google Cloud project (gemini / antigravity).
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Format {
    Kron,
    Codex,
    Claude,
    Google,
}

pub struct Auth {
    kind: String,
    path: PathBuf,
    format: Format,
    client_id: Option<String>,
    client_secret: Option<String>,
    state: tokio::sync::Mutex<Option<Creds>>,
}

impl Auth {
    pub fn new(cfg: &Config, name: &str, kind: &str, p: &ProviderCfg) -> Result<Auth> {
        let own = cfg.path("auth").join(format!("{name}.json"));
        let official = match kind {
            "codex" => Some((codex_home().join("auth.json"), Format::Codex)),
            "claude" => Some((claude_home().join(".credentials.json"), Format::Claude)),
            "gemini" => Some((user_home().join(".gemini").join("oauth_creds.json"), Format::Google)),
            _ => None,
        };
        let (path, format) = if let Some(c) = &p.credentials {
            let f = official.map(|o| o.1).unwrap_or(Format::Kron);
            (PathBuf::from(c), f)
        } else if own.exists() {
            (own, Format::Kron)
        } else if let Some((path, f)) = official.filter(|(p, _)| p.exists()) {
            (path, f)
        } else {
            (own, Format::Kron)
        };
        let client_id = p.client_id.clone().or(match kind {
            "codex" => Some(CODEX_CLIENT_ID.into()),
            "claude" => Some(CLAUDE_CLIENT_ID.into()),
            _ => None,
        });
        Ok(Auth {
            kind: kind.to_string(),
            path,
            format,
            client_id,
            client_secret: p.client_secret.clone(),
            state: tokio::sync::Mutex::new(None),
        })
    }

    /// A valid access token, refreshed if it is about to expire.
    pub async fn token(&self) -> Result<Creds> {
        let mut state = self.state.lock().await;
        let cached = match state.clone() {
            Some(c) => c,
            None => self.load()?,
        };
        let fresh = if expiring(&cached) { self.renew(&cached).await? } else { cached };
        *state = Some(fresh.clone());
        Ok(fresh)
    }

    /// Get a new token after the API rejected the current one.
    pub async fn refresh(&self) -> Result<Creds> {
        let mut state = self.state.lock().await;
        let mut cached = state.clone().unwrap_or_default();
        cached.expires_at = Some(0);
        let fresh = self.renew(&cached).await?;
        *state = Some(fresh.clone());
        Ok(fresh)
    }

    /// The official CLI may have refreshed (and rotated) the tokens on disk
    /// meanwhile: use those if they are newer, otherwise refresh ourselves.
    async fn renew(&self, cached: &Creds) -> Result<Creds> {
        let mut disk = self.load()?;
        disk.project = disk.project.or(cached.project.clone());
        if disk.access_token != cached.access_token && !expiring(&disk) {
            return Ok(disk);
        }
        self.do_refresh(&disk).await
    }

    pub async fn set_project(&self, project: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        if let Some(c) = state.as_mut() {
            c.project = Some(project.to_string());
            if self.format == Format::Kron {
                self.save(c)?;
            }
        }
        Ok(())
    }

    pub fn describe(&self) -> String {
        match self.load() {
            Ok(c) => {
                let exp = match c.expires_at {
                    Some(t) if t < now_ms() => " (expired, will refresh)".to_string(),
                    _ => String::new(),
                };
                format!("logged in via {}{exp}", self.path.display())
            }
            Err(_) => "not logged in (run `kron login`, or log in with the official CLI)".to_string(),
        }
    }

    fn load(&self) -> Result<Creds> {
        let text = std::fs::read_to_string(&self.path)
            .with_context(|| format!("no {} credentials at {} — run `kron login`", self.kind, self.path.display()))?;
        let v: Value = serde_json::from_str(&text)?;
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        let creds = match self.format {
            Format::Kron => serde_json::from_value(v)?,
            Format::Codex => {
                let t = &v["tokens"];
                let access = s(&t["access_token"]);
                Creds {
                    expires_at: jwt_exp(&access),
                    access_token: access,
                    refresh_token: s(&t["refresh_token"]),
                    account_id: t["account_id"]
                        .as_str()
                        .map(String::from)
                        .or_else(|| account_from_id_token(&s(&t["id_token"]))),
                    project: None,
                }
            }
            Format::Claude => {
                let o = &v["claudeAiOauth"];
                Creds {
                    access_token: s(&o["accessToken"]),
                    refresh_token: s(&o["refreshToken"]),
                    expires_at: o["expiresAt"].as_i64(),
                    ..Default::default()
                }
            }
            Format::Google => Creds {
                access_token: s(&v["access_token"]),
                refresh_token: s(&v["refresh_token"]),
                expires_at: v["expiry_date"].as_i64(),
                ..Default::default()
            },
        };
        if creds.access_token.is_empty() {
            bail!("{} has no access token", self.path.display());
        }
        Ok(creds)
    }

    /// Write tokens back, keeping the official files' other fields intact.
    fn save(&self, c: &Creds) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut v: Value =
            std::fs::read_to_string(&self.path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
        match self.format {
            Format::Kron => v = serde_json::to_value(c)?,
            Format::Codex => {
                v["tokens"]["access_token"] = json!(c.access_token);
                v["tokens"]["refresh_token"] = json!(c.refresh_token);
                v["last_refresh"] = json!(chrono::Utc::now().to_rfc3339());
            }
            Format::Claude => {
                v["claudeAiOauth"]["accessToken"] = json!(c.access_token);
                v["claudeAiOauth"]["refreshToken"] = json!(c.refresh_token);
                v["claudeAiOauth"]["expiresAt"] = json!(c.expires_at);
            }
            Format::Google => {
                v["access_token"] = json!(c.access_token);
                v["refresh_token"] = json!(c.refresh_token);
                v["expiry_date"] = json!(c.expires_at);
            }
        }
        write_private(&self.path, &serde_json::to_string_pretty(&v)?)
    }

    async fn do_refresh(&self, old: &Creds) -> Result<Creds> {
        if old.refresh_token.is_empty() {
            bail!("{} token expired and there is no refresh token — run `kron login`", self.kind);
        }
        let client_id = self.client_id.clone().unwrap_or_default();
        let resp: Value = match self.kind.as_str() {
            "codex" => {
                let body = json!({
                    "client_id": client_id, "grant_type": "refresh_token",
                    "refresh_token": old.refresh_token, "scope": "openid profile email",
                });
                check(http().post("https://auth.openai.com/oauth/token").json(&body).send().await?)
                    .await?
                    .json()
                    .await?
            }
            "claude" => {
                let body =
                    json!({"grant_type": "refresh_token", "refresh_token": old.refresh_token, "client_id": client_id});
                check(http().post("https://console.anthropic.com/v1/oauth/token").json(&body).send().await?)
                    .await?
                    .json()
                    .await?
            }
            _ => {
                let secret = self.client_secret.clone().with_context(|| {
                    format!("{} token expired: set client_id/client_secret for this provider (see README) or refresh it with the official CLI", self.kind)
                })?;
                let form = [
                    ("client_id", client_id.as_str()),
                    ("client_secret", secret.as_str()),
                    ("refresh_token", old.refresh_token.as_str()),
                    ("grant_type", "refresh_token"),
                ];
                check(http().post("https://oauth2.googleapis.com/token").form(&form).send().await?)
                    .await?
                    .json()
                    .await?
            }
        };
        let fresh = creds_from_token_response(&resp, Some(old));
        self.save(&fresh)?;
        Ok(fresh)
    }
}

fn expiring(c: &Creds) -> bool {
    c.expires_at.is_some_and(|t| t - now_ms() < 120_000)
}

fn creds_from_token_response(v: &Value, old: Option<&Creds>) -> Creds {
    let access = v["access_token"].as_str().unwrap_or("").to_string();
    let expires_at = v["expires_in"].as_i64().map(|s| now_ms() + s * 1000).or_else(|| jwt_exp(&access));
    Creds {
        refresh_token: v["refresh_token"]
            .as_str()
            .map(String::from)
            .or_else(|| old.map(|o| o.refresh_token.clone()))
            .unwrap_or_default(),
        account_id: v["id_token"]
            .as_str()
            .and_then(account_from_id_token)
            .or_else(|| old.and_then(|o| o.account_id.clone())),
        project: old.and_then(|o| o.project.clone()),
        access_token: access,
        expires_at,
    }
}

fn codex_home() -> PathBuf {
    std::env::var("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|_| user_home().join(".codex"))
}

fn claude_home() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|_| user_home().join(".claude"))
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&B64URL.decode(payload.trim_end_matches('=')).ok()?).ok()
}

fn jwt_exp(token: &str) -> Option<i64> {
    jwt_claims(token)?["exp"].as_i64().map(|s| s * 1000)
}

fn account_from_id_token(token: &str) -> Option<String> {
    jwt_claims(token)?["https://api.openai.com/auth"]["chatgpt_account_id"].as_str().map(String::from)
}

pub fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    std::fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    Ok(())
}

// ── Login ─────────────────────────────────────────────────────────────────────

/// Interactive OAuth login. Works without a local browser: open the printed
/// link anywhere, then paste back the URL you were redirected to.
pub async fn login(cfg: &Config, name: &str) -> Result<()> {
    let p = cfg.provider(name)?;
    let verifier = B64URL.encode(rand::random::<[u8; 32]>());
    let challenge = B64URL.encode(Sha256::digest(verifier.as_bytes()));
    let state = B64URL.encode(rand::random::<[u8; 16]>());

    let creds = match p.kind.as_str() {
        "codex" => {
            let redirect = "http://localhost:1455/auth/callback";
            let url = reqwest::Url::parse_with_params(
                "https://auth.openai.com/oauth/authorize",
                &[
                    ("response_type", "code"),
                    ("client_id", CODEX_CLIENT_ID),
                    ("redirect_uri", redirect),
                    ("scope", "openid profile email offline_access"),
                    ("code_challenge", &challenge),
                    ("code_challenge_method", "S256"),
                    ("id_token_add_organizations", "true"),
                    ("codex_cli_simplified_flow", "true"),
                    ("state", &state),
                    ("originator", "codex_cli_rs"),
                ],
            )?;
            let code = wait_for_code(url.as_str(), Some(1455)).await?;
            let form = [
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", redirect),
                ("client_id", CODEX_CLIENT_ID),
                ("code_verifier", verifier.as_str()),
            ];
            let v: Value = check(http().post("https://auth.openai.com/oauth/token").form(&form).send().await?)
                .await?
                .json()
                .await?;
            creds_from_token_response(&v, None)
        }
        "claude" => {
            let redirect = "https://console.anthropic.com/oauth/code/callback";
            let url = reqwest::Url::parse_with_params(
                "https://claude.ai/oauth/authorize",
                &[
                    ("code", "true"),
                    ("client_id", CLAUDE_CLIENT_ID),
                    ("response_type", "code"),
                    ("redirect_uri", redirect),
                    ("scope", "org:create_api_key user:profile user:inference"),
                    ("code_challenge", &challenge),
                    ("code_challenge_method", "S256"),
                    ("state", &verifier),
                ],
            )?;
            let pasted = wait_for_code(url.as_str(), None).await?;
            let (code, st) = pasted.split_once('#').unwrap_or((pasted.as_str(), verifier.as_str()));
            let body = json!({
                "code": code, "state": st, "grant_type": "authorization_code",
                "client_id": CLAUDE_CLIENT_ID, "redirect_uri": redirect, "code_verifier": verifier,
            });
            let v: Value = check(http().post("https://console.anthropic.com/v1/oauth/token").json(&body).send().await?)
                .await?
                .json()
                .await?;
            creds_from_token_response(&v, None)
        }
        "gemini" | "antigravity" => {
            let (client_id, secret) = match (&p.client_id, &p.client_secret) {
                (Some(i), Some(s)) => (i.clone(), s.clone()),
                _ => bail!("set client_id and client_secret for provider '{name}' first (see README)"),
            };
            let (port, path, extra) = if p.kind == "gemini" {
                (8085, "/oauth2callback", "")
            } else {
                (
                    51121,
                    "/oauth-callback",
                    " https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs",
                )
            };
            let redirect = format!("http://localhost:{port}{path}");
            let scope = format!(
                "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile{extra}"
            );
            let url = reqwest::Url::parse_with_params(
                "https://accounts.google.com/o/oauth2/v2/auth",
                &[
                    ("client_id", client_id.as_str()),
                    ("redirect_uri", &redirect),
                    ("response_type", "code"),
                    ("scope", &scope),
                    ("access_type", "offline"),
                    ("prompt", "consent"),
                    ("code_challenge", &challenge),
                    ("code_challenge_method", "S256"),
                    ("state", &state),
                ],
            )?;
            let code = wait_for_code(url.as_str(), Some(port)).await?;
            let form = [
                ("client_id", client_id.as_str()),
                ("client_secret", secret.as_str()),
                ("code", code.as_str()),
                ("code_verifier", verifier.as_str()),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect.as_str()),
            ];
            let v: Value = check(http().post("https://oauth2.googleapis.com/token").form(&form).send().await?)
                .await?
                .json()
                .await?;
            creds_from_token_response(&v, None)
        }
        other => bail!("provider '{name}' (kind {other}) uses an API key, no login needed"),
    };
    if creds.access_token.is_empty() {
        bail!("login failed: no access token returned");
    }
    let path = cfg.path("auth").join(format!("{name}.json"));
    std::fs::create_dir_all(path.parent().unwrap())?;
    write_private(&path, &serde_json::to_string_pretty(&creds)?)?;
    println!("Saved {}", path.display());
    Ok(())
}

/// Print the link, then take the code from the local redirect (if the browser
/// is on this machine) or from a pasted redirect URL / code.
async fn wait_for_code(url: &str, port: Option<u16>) -> Result<String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    println!("Open this link and sign in:\n\n{url}\n");
    println!("Then paste the URL you were redirected to (or the code) here:");

    let listener = match port {
        Some(p) => tokio::net::TcpListener::bind(("127.0.0.1", p)).await.ok(),
        None => None,
    };
    let from_browser = async {
        let Some(l) = listener else { return std::future::pending().await };
        loop {
            let Ok((mut sock, _)) = l.accept().await else { continue };
            let mut buf = vec![0u8; 8192];
            let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let target = req.split_whitespace().nth(1).unwrap_or("").to_string();
            if !target.contains("code=") {
                continue;
            }
            let body = "Signed in. You can close this tab.";
            let reply = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len());
            sock.write_all(reply.as_bytes()).await.ok();
            return format!("http://localhost{target}");
        }
    };
    let from_stdin = async {
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(l)) if !l.trim().is_empty() => return l.trim().to_string(),
                Ok(Some(_)) => continue,
                _ => return String::new(),
            }
        }
    };
    let got = tokio::select! { s = from_browser => s, s = from_stdin => s };
    extract_code(&got).context("no authorization code received")
}

pub fn extract_code(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.contains("code=") {
        let url = if s.contains("://") { s.to_string() } else { format!("http://x/?{}", s.trim_start_matches('?')) };
        let url = reqwest::Url::parse(&url).ok()?;
        return url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.to_string());
    }
    Some(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_from_redirect() {
        assert_eq!(extract_code("http://localhost:1455/auth/callback?code=abc&state=x").as_deref(), Some("abc"));
        assert_eq!(extract_code("code=q%2Fz&scope=1").as_deref(), Some("q/z"));
        assert_eq!(extract_code("raw#state").as_deref(), Some("raw#state"));
    }

    #[tokio::test]
    async fn picks_up_tokens_refreshed_by_official_cli() {
        let dir = std::env::temp_dir().join(format!("kron-auth-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("creds.json");
        let write = |tok: &str, exp: i64| {
            let v =
                json!({"claudeAiOauth": {"accessToken": tok, "refreshToken": "r", "expiresAt": exp, "scopes": ["x"]}});
            std::fs::write(&file, v.to_string()).unwrap();
        };
        write("old", now_ms() + 3_600_000);
        let p =
            ProviderCfg { kind: "claude".into(), credentials: Some(file.display().to_string()), ..Default::default() };
        let cfg = Config { home: dir.clone(), ..Default::default() };
        let auth = Auth::new(&cfg, "claude", "claude", &p).unwrap();
        assert_eq!(auth.token().await.unwrap().access_token, "old");

        // The official CLI refreshed meanwhile; our cached token is rejected.
        write("new", now_ms() + 3_600_000);
        assert_eq!(auth.refresh().await.unwrap().access_token, "new");
        let kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(kept["claudeAiOauth"]["scopes"][0], "x");
    }

    #[test]
    fn jwt_claims_decode() {
        let payload = B64URL.encode(br#"{"exp":100,"https://api.openai.com/auth":{"chatgpt_account_id":"acc"}}"#);
        let tok = format!("h.{payload}.s");
        assert_eq!(jwt_exp(&tok), Some(100_000));
        assert_eq!(account_from_id_token(&tok).as_deref(), Some("acc"));
    }
}
