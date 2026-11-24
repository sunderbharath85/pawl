//! Optional language model for wording (summaries, small talk).
//!
//! Providers, tried in order: Anthropic Messages API (plain reqwest POST
//! `https://api.anthropic.com/v1/messages`, headers `x-api-key` and
//! `anthropic-version: 2023-06-01`), then Ollama `POST /api/chat` with `stream: false`.
//! Output is plain text only; callers never derive actions from it.

use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use serde_json::{json, Value};

use super::{BrainConfig, DEFAULT_MODEL};
use crate::util::truncate;

pub(crate) const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";
const ANTHROPIC_TIMEOUT: Duration = Duration::from_secs(30);
const OLLAMA_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REPLY_CHARS: usize = 700;

#[derive(Clone)]
enum Provider {
    Anthropic { api_key: String, model: String },
    Ollama { url: String, model: String },
}

impl Provider {
    fn label(&self) -> String {
        match self {
            Provider::Anthropic { model, .. } => format!("anthropic:{model}"),
            Provider::Ollama { model, .. } => format!("ollama:{model}"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Llm {
    http: reqwest::Client,
    providers: Vec<Provider>,
}

impl std::fmt::Debug for Llm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Llm").field("providers", &self.describe()).finish()
    }
}

/// `http://host:11434` / `host:11434` / `.../api` / `.../api/chat` -> full chat URL.
pub(crate) fn ollama_chat_url(base: &str) -> String {
    let mut u = base.trim().trim_end_matches('/').to_string();
    if !u.contains("://") {
        u = format!("http://{u}");
    }
    if u.ends_with("/api/chat") {
        u
    } else if u.ends_with("/api") {
        format!("{u}/chat")
    } else {
        format!("{u}/api/chat")
    }
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

impl Llm {
    /// `None` when no model is configured (deterministic mode) or the HTTP client fails.
    pub fn from_config(cfg: &BrainConfig) -> Option<Self> {
        let mut providers = Vec::new();
        if let Some(key) = non_empty(cfg.anthropic_api_key.as_deref()) {
            let model = non_empty(Some(cfg.model.as_str())).unwrap_or(DEFAULT_MODEL);
            providers.push(Provider::Anthropic { api_key: key.to_string(), model: model.to_string() });
        }
        if let (Some(url), Some(model)) = (non_empty(cfg.ollama_url.as_deref()), non_empty(cfg.ollama_model.as_deref())) {
            providers.push(Provider::Ollama { url: ollama_chat_url(url), model: model.to_string() });
        }
        if providers.is_empty() {
            return None;
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("krypto-brain/", env!("CARGO_PKG_VERSION")))
            .build();
        match http {
            Ok(http) => Some(Self { http, providers }),
            Err(e) => {
                tracing::warn!(error = %e, "brain: HTTP client unavailable; using deterministic replies");
                None
            }
        }
    }

    pub fn describe(&self) -> String {
        self.providers.iter().map(Provider::label).collect::<Vec<_>>().join(", ")
    }

    /// Ask the first provider that answers. Errors are logged (never with secrets).
    pub async fn complete(&self, system: &str, user: &str, max_tokens: u32) -> anyhow::Result<String> {
        let mut last_err = None;
        for p in &self.providers {
            let res = match p {
                Provider::Anthropic { api_key, model } => {
                    anthropic(&self.http, api_key, model, system, user, max_tokens).await
                }
                Provider::Ollama { url, model } => ollama(&self.http, url, model, system, user, max_tokens).await,
            };
            match res.map(|t| clean_output(&t)) {
                Ok(t) if !t.is_empty() => return Ok(t),
                Ok(_) => last_err = Some(anyhow!("{} returned an empty reply", p.label())),
                Err(e) => {
                    tracing::warn!(provider = %p.label(), error = %e, "brain: LLM call failed");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no LLM provider configured")))
    }
}

fn api_error(v: &Value, body: &str) -> String {
    let msg = v
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| v.get("error").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| body.to_string());
    truncate(&msg, 200)
}

async fn anthropic(
    http: &reqwest::Client,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> anyhow::Result<String> {
    let body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [{ "role": "user", "content": user }],
    });
    let resp = http
        .post(ANTHROPIC_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&body)
        .timeout(ANTHROPIC_TIMEOUT)
        .send()
        .await
        .context("anthropic request failed")?;
    let status = resp.status();
    let text = resp.text().await.context("anthropic response unreadable")?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("anthropic HTTP {}: {}", status.as_u16(), api_error(&v, &text));
    }
    Ok(anthropic_text(&v))
}

/// Concatenate the `text` blocks of a Messages API response.
pub(crate) fn anthropic_text(v: &Value) -> String {
    v.get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

async fn ollama(
    http: &reqwest::Client,
    url: &str,
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> anyhow::Result<String> {
    let body = json!({
        "model": model,
        "stream": false,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "options": { "num_predict": max_tokens.max(128) * 4 },
    });
    let resp = http
        .post(url)
        .json(&body)
        .timeout(OLLAMA_TIMEOUT)
        .send()
        .await
        .context("ollama request failed")?;
    let status = resp.status();
    let text = resp.text().await.context("ollama response unreadable")?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("ollama HTTP {}: {}", status.as_u16(), api_error(&v, &text));
    }
    Ok(v.pointer("/message/content").and_then(Value::as_str).unwrap_or_default().to_string())
}

/// Strip `<think>` blocks and wrapping quotes, tidy whitespace, cap length.
pub(crate) fn clean_output(raw: &str) -> String {
    let mut s = raw.to_string();
    while let Some(start) = s.find("<think>") {
        match s[start..].find("</think>") {
            Some(end) => s.replace_range(start..start + end + "</think>".len(), ""),
            None => s.truncate(start),
        }
    }
    let mut s = s.trim().to_string();
    for (open, close) in [('"', '"'), ('“', '”')] {
        if s.len() >= 2 && s.starts_with(open) && s.ends_with(close) && s.matches(open).count() <= 2 {
            s = s[open.len_utf8()..s.len() - close.len_utf8()].trim().to_string();
        }
    }
    let lines: Vec<&str> = s.lines().map(str::trim_end).collect();
    let mut tidy = String::new();
    let mut blank = 0;
    for l in lines {
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        if !tidy.is_empty() {
            tidy.push('\n');
        }
        tidy.push_str(l);
    }
    truncate(tidy.trim(), MAX_REPLY_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_urls() {
        assert_eq!(ollama_chat_url("http://127.0.0.1:11434"), "http://127.0.0.1:11434/api/chat");
        assert_eq!(ollama_chat_url("localhost:11434/"), "http://localhost:11434/api/chat");
        assert_eq!(ollama_chat_url("http://h/api"), "http://h/api/chat");
        assert_eq!(ollama_chat_url("http://h/api/chat"), "http://h/api/chat");
    }

    #[test]
    fn parses_anthropic_content_blocks() {
        let v = json!({"content": [{"type": "text", "text": "Woof! "}, {"type": "tool_use"}, {"type": "text", "text": "Hi."}]});
        assert_eq!(anthropic_text(&v), "Woof! Hi.");
        assert_eq!(anthropic_text(&json!({})), "");
    }

    #[test]
    fn cleans_output() {
        assert_eq!(clean_output("<think>hmm</think>\n\n\"Woof! All good.\""), "Woof! All good.");
        assert_eq!(clean_output("a\n\n\n\nb"), "a\n\nb");
        assert_eq!(clean_output("<think>never closed"), "");
        assert!(clean_output(&"x".repeat(2000)).chars().count() <= MAX_REPLY_CHARS);
    }

    #[test]
    fn no_providers_means_no_llm() {
        assert!(Llm::from_config(&BrainConfig::default()).is_none());
        let cfg = BrainConfig { anthropic_api_key: Some("  ".into()), ..BrainConfig::default() };
        assert!(Llm::from_config(&cfg).is_none());
        let cfg = BrainConfig { ollama_url: Some("http://127.0.0.1:11434".into()), ..BrainConfig::default() };
        assert!(Llm::from_config(&cfg).is_none(), "ollama needs a model too");
        let cfg = BrainConfig {
            anthropic_api_key: Some("k".into()),
            ollama_url: Some("http://127.0.0.1:11434".into()),
            ollama_model: Some("llama3.2".into()),
            ..BrainConfig::default()
        };
        let llm = Llm::from_config(&cfg).unwrap();
        assert_eq!(llm.describe(), "anthropic:claude-haiku-5-5, ollama:llama3.2");
        assert!(!format!("{llm:?}").contains("\"k\""));
    }
}
