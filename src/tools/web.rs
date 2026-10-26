//! web_fetch: read one page as text (the model's own web_search finds pages,
//! this reads them).

use anyhow::{Result, bail};
use regex::Regex;
use std::sync::OnceLock;

const MAX_BYTES: usize = 5_000_000;

pub async fn fetch(url: &str, offset: usize, max_chars: usize) -> Result<String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("only http(s) URLs can be fetched");
    }
    let resp = crate::llm::http()
        .get(url)
        .header("user-agent", "Mozilla/5.0 (compatible; kron; +https://github.com/k0de1ne/KRON)")
        .header("accept", "text/html,application/xhtml+xml,text/plain,application/json;q=0.9,*/*;q=0.5")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;
    let status = resp.status();
    let kind = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    let final_url = resp.url().to_string();
    let bytes = resp.bytes().await?;
    let bytes = &bytes[..bytes.len().min(MAX_BYTES)];
    let textual = kind.is_empty() || kind.contains("text") || kind.contains("json") || kind.contains("xml");
    if !textual {
        return Ok(format!("{final_url}\nHTTP {status}, {kind}, {} bytes: not a text page.", bytes.len()));
    }
    let raw = String::from_utf8_lossy(bytes);
    let text = if kind.contains("html") || raw.trim_start().starts_with('<') { html_to_text(&raw) } else { raw.into() };
    let total = text.chars().count();
    let mut out = format!("{final_url}\nHTTP {status}\n\n");
    out.extend(text.chars().skip(offset).take(max_chars));
    if offset + max_chars < total {
        out.push_str(&format!(
            "\n\n[… {} more characters: call again with offset {}]",
            total - offset - max_chars,
            offset + max_chars
        ));
    }
    Ok(out)
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap())
}

/// Readable text of an HTML page: title, headings, paragraphs and links.
pub fn html_to_text(html: &str) -> String {
    static DROP: OnceLock<Regex> = OnceLock::new();
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static LINK: OnceLock<Regex> = OnceLock::new();
    static HEAD: OnceLock<Regex> = OnceLock::new();
    static ITEM: OnceLock<Regex> = OnceLock::new();
    static BREAK: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    static SPACES: OnceLock<Regex> = OnceLock::new();
    static LINES: OnceLock<Regex> = OnceLock::new();

    let title = re(&TITLE, r"(?is)<title[^>]*>(.*?)</title>")
        .captures(html)
        .map(|c| decode(c[1].trim()))
        .filter(|t| !t.is_empty());
    let s = re(&DROP, r"(?is)<(script|style|noscript|svg|template|head|iframe)\b.*?</(script|style|noscript|svg|template|head|iframe)\s*>|<!--.*?-->")
        .replace_all(html, " ");
    let s = re(&LINK, r#"(?is)<a\b[^>]*?href\s*=\s*["']([^"'#][^"']*)["'][^>]*>(.*?)</a>"#).replace_all(
        &s,
        |c: &regex::Captures| {
            let label = c[2].to_string();
            let href = &c[1];
            if href.starts_with("javascript:") { label } else { format!("[{label}]({href})") }
        },
    );
    let s = re(&HEAD, r"(?is)<h([1-6])\b[^>]*>")
        .replace_all(&s, |c: &regex::Captures| format!("\n\n{} ", "#".repeat(c[1].parse().unwrap_or(1))));
    let s = re(&ITEM, r"(?i)<li\b[^>]*>").replace_all(&s, "\n- ");
    let s = re(&BREAK, r"(?i)<br\s*/?>|</?(p|div|section|article|header|footer|main|nav|aside|tr|table|ul|ol|pre|blockquote|h[1-6]|dd|dt|figure|form)\b[^>]*>")
        .replace_all(&s, "\n");
    let s = re(&TAG, r"(?s)<[^>]*>").replace_all(&s, "");
    let s = decode(&s);
    let s = re(&SPACES, r"[ \t\u{a0}]+").replace_all(&s, " ");
    let s: String = s.lines().map(str::trim).collect::<Vec<_>>().join("\n");
    let s = re(&LINES, r"\n{3,}").replace_all(&s, "\n\n");
    match title {
        Some(t) => format!("Title: {t}\n\n{}", s.trim()),
        None => s.trim().to_string(),
    }
}

fn decode(s: &str) -> String {
    static ENTITY: OnceLock<Regex> = OnceLock::new();
    re(&ENTITY, r"&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);")
        .replace_all(s, |c: &regex::Captures| {
            let e = &c[1];
            let ch = if let Some(hex) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = e.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                match e {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    "mdash" => Some('—'),
                    "ndash" => Some('–'),
                    "hellip" => Some('…'),
                    "laquo" => Some('«'),
                    "raquo" => Some('»'),
                    "copy" => Some('©'),
                    _ => None,
                }
            };
            ch.map(String::from).unwrap_or_else(|| c[0].to_string())
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_text() {
        let html = r#"<html><head><title>Hi &amp; bye</title><style>p{}</style></head>
            <body><script>alert(1)</script><h2>News</h2><p>One&nbsp;two <a href="/x">link</a></p>
            <ul><li>a</li><li>b</li></ul></body></html>"#;
        let t = html_to_text(html);
        assert!(t.starts_with("Title: Hi & bye"), "{t}");
        assert!(t.contains("## News"), "{t}");
        assert!(t.contains("One two [link](/x)"), "{t}");
        assert!(t.contains("- a\n- b"), "{t}");
        assert!(!t.contains("alert"), "{t}");
    }
}
