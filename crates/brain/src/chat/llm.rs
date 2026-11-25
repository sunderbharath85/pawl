//! Optional language model for wording (summaries, small talk) and for understanding free-form
//! messages ([`Llm::complete_json`], one structured intent the caller validates).
//!
//! One provider answers: the configured `provider`, or with Auto the first configured of
//! Anthropic, OpenAI, Gemini, Ollama. Plain reqwest, no SDKs:
//! - Anthropic Messages API: `POST https://api.anthropic.com/v1/messages`, headers `x-api-key` and
//!   `anthropic-version: 2023-06-01`; JSON via `output_config.format = {type: "json_schema", schema}`
//!   (the JSON is the text block).
//! - OpenAI Chat Completions: `POST https://api.openai.com/v1/chat/completions`, `Authorization: Bearer`;
//!   JSON via `response_format = {type: "json_schema", json_schema: {name, strict: true, schema}}`;
//!   text in `choices[0].message.content` (`refusal` set = no answer).
//! - Gemini generateContent: `POST https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent`,
//!   header `x-goog-api-key`; `systemInstruction`, `contents`, `generationConfig.maxOutputTokens`; JSON via
//!   `generationConfig.responseMimeType = "application/json"` + `responseJsonSchema`; text in
//!   `candidates[0].content.parts[].text` (thought parts skipped). Stateless (the newer Interactions API
//!   stores conversations server-side by default).
//! - Ollama `POST /api/chat` with `stream: false`; JSON via `format: <schema>`.
//! - ACP: a [`ModelSession`] the hub runs (an installed coding agent, e.g. Claude Code + Sonnet, on the
//!   user's own login, every tool denied); JSON is asked for in the instructions and parsed leniently.
//!
//! Output is text or a JSON value only; callers never derive actions from it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use pawl_protocol::{BotProvider, ChatImage};
use serde_json::{json, Value};

use super::vision;
use super::{BrainConfig, DEFAULT_GEMINI_MODEL, DEFAULT_MODEL, DEFAULT_OPENAI_MODEL};
use crate::util::truncate;

pub(crate) const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";
pub(crate) const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";
pub(crate) const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const CLOUD_TIMEOUT: Duration = Duration::from_secs(30);
const OLLAMA_TIMEOUT: Duration = Duration::from_secs(60);
/// One turn of the hub's warm agent session (an agent is slower than a plain API call).
const ACP_TIMEOUT: Duration = Duration::from_secs(45);
/// A turn with a picture: the upload and the model's look take longer.
const VISION_TIMEOUT: Duration = Duration::from_secs(60);
const ACP_VISION_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_REPLY_CHARS: usize = 700;
/// Name of the structured output (OpenAI requires one).
const SCHEMA_NAME: &str = "pet_intent";
/// Largest provider response read (a declared larger body is refused before reading it).
pub(crate) const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// The lowest documented reasoning effort for short pet replies (Chat Completions `reasoning_effort`:
/// none, minimal, low, …; "Not all reasoning models support every value"). GPT-6 models list `none`;
/// GPT-5 models `minimal`; o-series `low`. Other models get none sent (their default).
pub(crate) fn openai_reasoning_effort(model: &str) -> Option<&'static str> {
    let m = model.trim().to_ascii_lowercase();
    if m.starts_with("gpt-6") {
        Some("none")
    } else if m.starts_with("gpt-5") {
        Some("minimal")
    } else if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        Some("low")
    } else {
        None
    }
}

/// The cheapest documented thinking setting: `thinkingLevel: MINIMAL` on Gemini 3 and later ("Use with
/// earlier models results in an error"), `thinkingBudget: 0` on 2.5 Flash models; nothing elsewhere.
pub(crate) fn gemini_thinking(model: &str) -> Option<Value> {
    let m = model.trim().to_ascii_lowercase();
    let major = m.strip_prefix("gemini-").and_then(|r| r.split(['.', '-']).next()).and_then(|v| v.parse::<u32>().ok());
    match major {
        Some(n) if n >= 3 => Some(json!({ "thinkingLevel": "MINIMAL" })),
        Some(2) if m.starts_with("gemini-2.5-flash") => Some(json!({ "thinkingBudget": 0 })),
        _ => None,
    }
}

/// The body of `resp`, at most [`MAX_RESPONSE_BYTES`] (refused when it declares or turns out larger).
async fn read_capped(mut resp: reqwest::Response, name: &str) -> anyhow::Result<String> {
    if resp.content_length().is_some_and(|n| n > MAX_RESPONSE_BYTES as u64) {
        bail!("{name} response is larger than {} KiB", MAX_RESPONSE_BYTES / 1024);
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.with_context(|| format!("{name} response unreadable"))? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            bail!("{name} response is larger than {} KiB", MAX_RESPONSE_BYTES / 1024);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// The future of one [`ModelSession::complete`] call.
pub type ModelFuture<'a> = Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + 'a>>;

/// A model reached through a session the hub runs (the "acp" provider: an installed agent such as Claude
/// Code on the user's own login). The brain never depends on the hub: the hub implements this, and must
/// keep the session tool-less (every tool denied, every permission request rejected).
pub trait ModelSession: Send + Sync {
    /// Short label for logs and `describe`, e.g. "claude-code:sonnet". Never a secret.
    fn label(&self) -> String;
    /// One turn: instructions, then the message; plain text back, or an error after `timeout`.
    fn complete<'a>(&'a self, system: &'a str, user: &'a str, timeout: Duration) -> ModelFuture<'a>;
    /// Does the session's agent take pictures (ACP `promptCapabilities.image`)? Default: no.
    fn sees_images(&self) -> bool {
        false
    }
    /// One turn with a picture (an ACP image block before the text). The picture is used for this turn
    /// only. Default: refused.
    fn complete_with_image<'a>(&'a self, _system: &'a str, _user: &'a str, _image: &'a ChatImage, _timeout: Duration) -> ModelFuture<'a> {
        Box::pin(async { Err(anyhow!("this model session takes no pictures")) })
    }
}

#[derive(Clone)]
pub(crate) enum Provider {
    Anthropic { api_key: String, model: String, url: String },
    OpenAi { api_key: String, model: String, url: String },
    Gemini { api_key: String, model: String, base: String },
    Ollama { url: String, model: String },
    Acp(Arc<dyn ModelSession>),
}

impl Provider {
    pub(crate) fn kind(&self) -> BotProvider {
        match self {
            Provider::Anthropic { .. } => BotProvider::Anthropic,
            Provider::OpenAi { .. } => BotProvider::Openai,
            Provider::Gemini { .. } => BotProvider::Gemini,
            Provider::Ollama { .. } => BotProvider::Ollama,
            Provider::Acp(_) => BotProvider::Acp,
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Provider::Anthropic { model, .. } => format!("anthropic:{model}"),
            Provider::OpenAi { model, .. } => format!("openai:{model}"),
            Provider::Gemini { model, .. } => format!("gemini:{model}"),
            Provider::Ollama { model, .. } => format!("ollama:{model}"),
            Provider::Acp(s) => format!("acp:{}", s.label()),
        }
    }

    fn timeout(&self) -> Duration {
        match self {
            Provider::Ollama { .. } => OLLAMA_TIMEOUT,
            Provider::Acp(_) => ACP_TIMEOUT,
            _ => CLOUD_TIMEOUT,
        }
    }
}

/// Every provider `cfg` configures, in the Auto order: Anthropic, OpenAI, Gemini, Ollama, then the hub's
/// agent session (`acp`: given only while its agent is installed).
pub(crate) fn configured(cfg: &BrainConfig, acp: Option<&Arc<dyn ModelSession>>) -> impl Iterator<Item = Provider> {
    let model = |m: &str, default: &str| non_empty(Some(m)).unwrap_or(default).to_string();
    let mut v = Vec::new();
    if let Some(key) = non_empty(cfg.anthropic_api_key.as_deref()) {
        v.push(Provider::Anthropic { api_key: key.into(), model: model(&cfg.model, DEFAULT_MODEL), url: ANTHROPIC_URL.into() });
    }
    if let Some(key) = non_empty(cfg.openai_api_key.as_deref()) {
        v.push(Provider::OpenAi { api_key: key.into(), model: model(&cfg.openai_model, DEFAULT_OPENAI_MODEL), url: OPENAI_URL.into() });
    }
    if let Some(key) = non_empty(cfg.gemini_api_key.as_deref()) {
        v.push(Provider::Gemini { api_key: key.into(), model: model(&cfg.gemini_model, DEFAULT_GEMINI_MODEL), base: GEMINI_BASE.into() });
    }
    if let (Some(url), Some(m)) = (non_empty(cfg.ollama_url.as_deref()), non_empty(cfg.ollama_model.as_deref())) {
        v.push(Provider::Ollama { url: ollama_chat_url(url), model: m.to_string() });
    }
    if let Some(s) = acp {
        v.push(Provider::Acp(Arc::clone(s)));
    }
    v.into_iter()
}

/// The provider that answers: the chosen one when it is configured (else none: the user picked it on
/// purpose), or with Auto the first configured.
pub(crate) fn select(cfg: &BrainConfig, acp: Option<&Arc<dyn ModelSession>>) -> Option<Provider> {
    match cfg.provider {
        BotProvider::Auto => configured(cfg, acp).next(),
        want => configured(cfg, acp).find(|p| p.kind() == want),
    }
}

#[derive(Clone)]
pub(crate) struct Llm {
    http: reqwest::Client,
    provider: Provider,
}

impl std::fmt::Debug for Llm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Llm").field("provider", &self.describe()).finish()
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

/// A model id that is safe as a URL path segment (Gemini puts it in the path).
fn path_safe(model: &str) -> bool {
    !model.is_empty() && model.chars().all(|c| c.is_ascii_alphanumeric() || "._-:".contains(c))
}

impl Llm {
    /// `None` when no model is configured (deterministic mode) or the HTTP client fails.
    pub fn from_config(cfg: &BrainConfig, acp: Option<&Arc<dyn ModelSession>>) -> Option<Self> {
        Self::with_provider(select(cfg, acp)?)
    }

    pub(crate) fn with_provider(provider: Provider) -> Option<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("pawl-brain/", env!("CARGO_PKG_VERSION")))
            .build();
        match http {
            Ok(http) => Some(Self { http, provider }),
            Err(e) => {
                tracing::warn!(error = %e, "brain: HTTP client unavailable; using deterministic replies");
                None
            }
        }
    }

    pub fn describe(&self) -> String {
        self.provider.label()
    }

    /// Reply text from the provider. Errors are logged (never with secrets).
    pub async fn complete(&self, system: &str, user: &str, max_tokens: u32) -> anyhow::Result<String> {
        let raw = self.call(system, user, max_tokens, None).await.inspect_err(|e| {
            tracing::warn!(provider = %self.describe(), error = %e, "brain: LLM call failed");
        })?;
        let t = clean_output(&raw);
        if t.is_empty() {
            bail!("{} returned an empty reply", self.describe());
        }
        Ok(t)
    }

    /// Can this provider see pictures? By model name (Anthropic, OpenAI), always (Gemini), the server's
    /// answer (Ollama `/api/show` capabilities), or the agent's (ACP `promptCapabilities.image`).
    pub async fn sees_images(&self) -> bool {
        match &self.provider {
            Provider::Anthropic { model, .. } => vision::anthropic_sees(model),
            Provider::OpenAi { model, .. } => vision::openai_sees(model),
            Provider::Gemini { model, .. } => path_safe(model),
            Provider::Ollama { url, model } => {
                let show = self.http.post(vision::ollama_show_url(url)).json(&json!({ "model": model })).timeout(Duration::from_secs(10)).send().await;
                let body = match show {
                    Ok(r) if r.status().is_success() => read_capped(r, "ollama").await.ok(),
                    _ => None,
                };
                body.and_then(|b| serde_json::from_str::<Value>(&b).ok()).is_some_and(|v| vision::ollama_shows_vision(&v))
            }
            Provider::Acp(s) => s.sees_images(),
        }
    }

    /// A reply about one picture (used for this call only: never stored or logged). Check
    /// [`Self::sees_images`] first.
    pub async fn look(&self, system: &str, user: &str, image: &ChatImage, max_tokens: u32) -> anyhow::Result<String> {
        let raw = match &self.provider {
            Provider::Acp(s) => s.complete_with_image(system, user, image, ACP_VISION_TIMEOUT).await,
            _ => self.call_with(system, user, max_tokens, None, Some(image)).await,
        }
        .inspect_err(|e| tracing::warn!(provider = %self.describe(), error = %e, "brain: LLM call with a picture failed"))?;
        let t = clean_output(&raw);
        if t.is_empty() {
            bail!("{} returned an empty reply", self.describe());
        }
        Ok(t)
    }

    /// One JSON object shaped by `schema` (the provider's structured output). The caller validates
    /// every field: the schema only shapes the answer.
    pub async fn complete_json(&self, system: &str, user: &str, schema: &Value, max_tokens: u32) -> anyhow::Result<Value> {
        let raw = self.call(system, user, max_tokens, Some(schema)).await.inspect_err(|e| {
            tracing::warn!(provider = %self.describe(), error = %e, "brain: LLM call failed");
        })?;
        parse_json_reply(&raw)
    }

    async fn call(&self, system: &str, user: &str, max_tokens: u32, schema: Option<&Value>) -> anyhow::Result<String> {
        self.call_with(system, user, max_tokens, schema, None).await
    }

    async fn call_with(&self, system: &str, user: &str, max_tokens: u32, schema: Option<&Value>, image: Option<&ChatImage>) -> anyhow::Result<String> {
        let p = &self.provider;
        if let Provider::Acp(s) = p {
            // No structured output over ACP: the schema goes into the instructions.
            let system = match schema {
                Some(schema) => format!(
                    "{system}\n\nReply with exactly one JSON object and nothing else. It must match this JSON schema:\n{schema}"
                ),
                None => system.to_string(),
            };
            return s.complete(&system, user, p.timeout()).await;
        }
        let body = |b: Value| match image {
            Some(img) => with_image(p, b, img),
            None => b,
        };
        let req = match p {
            Provider::Anthropic { api_key, model, url } => self
                .http
                .post(url)
                .header("x-api-key", api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body(anthropic_body(model, system, user, max_tokens, schema))),
            Provider::OpenAi { api_key, model, url } => {
                self.http.post(url).bearer_auth(api_key).json(&body(openai_body(model, system, user, max_tokens, schema)))
            }
            Provider::Gemini { api_key, model, base } => {
                if !path_safe(model) {
                    bail!("gemini model id {:?} is not a plain model name", truncate(model, 40));
                }
                self.http
                    .post(format!("{}/{model}:generateContent", base.trim_end_matches('/')))
                    .header("x-goog-api-key", api_key)
                    .json(&body(gemini_body(model, system, user, max_tokens, schema)))
            }
            Provider::Ollama { url, model } => self.http.post(url).json(&body(ollama_body(model, system, user, max_tokens, schema))),
            Provider::Acp(_) => unreachable!("answered above"),
        };
        let name = p.label();
        let timeout = if image.is_some() { VISION_TIMEOUT.max(p.timeout()) } else { p.timeout() };
        let resp = req.timeout(timeout).send().await.with_context(|| format!("{name} request failed"))?;
        let status = resp.status();
        let text = read_capped(resp, &name).await?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("{name} HTTP {}: {}", status.as_u16(), api_error(&v, &text));
        }
        match p {
            Provider::Anthropic { .. } => Ok(anthropic_text(&v)),
            Provider::OpenAi { .. } => openai_text(&v),
            Provider::Gemini { .. } => gemini_text(&v),
            Provider::Ollama { .. } => Ok(v.pointer("/message/content").and_then(Value::as_str).unwrap_or_default().to_string()),
            Provider::Acp(_) => unreachable!("answered above"),
        }
    }
}

/// `body` (a provider's text request) with `image` added to its user message, the way each API takes
/// pictures (see [`vision`]). The user's text stays, after the picture (Anthropic, Gemini) or with it.
pub(crate) fn with_image(p: &Provider, mut body: Value, image: &ChatImage) -> Value {
    let (mt, data) = (image.media_type.as_str(), image.data.as_str());
    match p {
        Provider::Anthropic { .. } => {
            let text = body["messages"][0]["content"].clone();
            body["messages"][0]["content"] = json!([
                { "type": "image", "source": { "type": "base64", "media_type": mt, "data": data } },
                { "type": "text", "text": text },
            ]);
        }
        Provider::OpenAi { .. } => {
            let text = body["messages"][1]["content"].clone();
            body["messages"][1]["content"] = json!([
                { "type": "text", "text": text },
                { "type": "image_url", "image_url": { "url": format!("data:{mt};base64,{data}") } },
            ]);
        }
        Provider::Gemini { .. } => {
            let text = body["contents"][0]["parts"][0]["text"].clone();
            body["contents"][0]["parts"] = json!([{ "inline_data": { "mime_type": mt, "data": data } }, { "text": text }]);
        }
        Provider::Ollama { .. } => body["messages"][1]["images"] = json!([data]),
        Provider::Acp(_) => {}
    }
    body
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

pub(crate) fn anthropic_body(model: &str, system: &str, user: &str, max_tokens: u32, schema: Option<&Value>) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [{ "role": "user", "content": user }],
    });
    if let Some(schema) = schema {
        body["output_config"] = json!({ "format": { "type": "json_schema", "schema": schema } });
    }
    body
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

/// `max_completion_tokens` also counts reasoning tokens, so the budget gets headroom.
pub(crate) fn openai_body(model: &str, system: &str, user: &str, max_tokens: u32, schema: Option<&Value>) -> Value {
    let mut body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "max_completion_tokens": max_tokens.saturating_mul(4).max(512),
    });
    if let Some(effort) = openai_reasoning_effort(model) {
        body["reasoning_effort"] = json!(effort);
    }
    if let Some(schema) = schema {
        body["response_format"] = json!({ "type": "json_schema", "json_schema": { "name": SCHEMA_NAME, "strict": true, "schema": schema } });
    }
    body
}

/// `choices[0].message.content`; a refusal is an error (no answer).
pub(crate) fn openai_text(v: &Value) -> anyhow::Result<String> {
    let msg = v.pointer("/choices/0/message").ok_or_else(|| anyhow!("openai reply has no message"))?;
    if let Some(r) = msg.get("refusal").and_then(Value::as_str).filter(|r| !r.trim().is_empty()) {
        bail!("openai refused: {}", truncate(r, 120));
    }
    Ok(msg.get("content").and_then(Value::as_str).unwrap_or_default().to_string())
}

/// Thinking tokens count toward `maxOutputTokens` on thinking models, so the budget gets headroom.
pub(crate) fn gemini_body(model: &str, system: &str, user: &str, max_tokens: u32, schema: Option<&Value>) -> Value {
    let mut config = json!({ "maxOutputTokens": max_tokens.saturating_mul(4).max(512) });
    if let Some(thinking) = gemini_thinking(model) {
        config["thinkingConfig"] = thinking;
    }
    if let Some(schema) = schema {
        config["responseMimeType"] = json!("application/json");
        config["responseJsonSchema"] = schema.clone();
    }
    json!({
        "systemInstruction": { "parts": [{ "text": system }] },
        "contents": [{ "role": "user", "parts": [{ "text": user }] }],
        "generationConfig": config,
    })
}

/// The first candidate's text parts (thought summaries skipped); a blocked prompt is an error.
pub(crate) fn gemini_text(v: &Value) -> anyhow::Result<String> {
    if let Some(reason) = v.pointer("/promptFeedback/blockReason").and_then(Value::as_str) {
        bail!("gemini blocked the prompt ({reason})");
    }
    let parts = v.pointer("/candidates/0/content/parts").and_then(Value::as_array).ok_or_else(|| anyhow!("gemini reply has no candidate"))?;
    Ok(parts
        .iter()
        .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(""))
}

pub(crate) fn ollama_body(model: &str, system: &str, user: &str, max_tokens: u32, schema: Option<&Value>) -> Value {
    let mut body = json!({
        "model": model,
        "stream": false,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "options": { "num_predict": max_tokens.max(128) * 4 },
    });
    if let Some(schema) = schema {
        body["format"] = schema.clone();
    }
    body
}

/// The JSON object of a structured reply (tolerates a ```json fence and `<think>` blocks).
pub(crate) fn parse_json_reply(raw: &str) -> anyhow::Result<Value> {
    let mut s = raw.to_string();
    while let Some(start) = s.find("<think>") {
        match s[start..].find("</think>") {
            Some(end) => s.replace_range(start..start + end + "</think>".len(), ""),
            None => s.truncate(start),
        }
    }
    let t = s.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t).trim();
    let v: Value = serde_json::from_str(t).context("the model's reply is not JSON")?;
    if !v.is_object() {
        bail!("the model's reply is not a JSON object");
    }
    Ok(v)
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

    fn cfg_with(f: impl FnOnce(&mut BrainConfig)) -> BrainConfig {
        let mut c = BrainConfig::default();
        f(&mut c);
        c
    }

    #[test]
    fn provider_selection() {
        assert!(Llm::from_config(&BrainConfig::default(), None).is_none());
        assert!(Llm::from_config(&cfg_with(|c| c.anthropic_api_key = Some("  ".into())), None).is_none());
        assert!(Llm::from_config(&cfg_with(|c| c.ollama_url = Some("http://127.0.0.1:11434".into())), None).is_none(), "ollama needs a model too");
        let all = cfg_with(|c| {
            c.anthropic_api_key = Some("ka".into());
            c.openai_api_key = Some("ko".into());
            c.gemini_api_key = Some("kg".into());
            c.ollama_url = Some("http://127.0.0.1:11434".into());
            c.ollama_model = Some("llama3.2".into());
        });
        // Auto: the first configured, in the order anthropic, openai, gemini, ollama.
        let labels: Vec<String> = configured(&all, None).map(|p| p.label()).collect();
        assert_eq!(labels, ["anthropic:claude-haiku-5-5", "openai:gpt-6-luna", "gemini:gemini-3.5-flash-lite", "ollama:llama3.2"]);
        assert_eq!(Llm::from_config(&all, None).unwrap().describe(), "anthropic:claude-haiku-5-5");
        let no_anthropic = cfg_with(|c| {
            c.openai_api_key = Some("ko".into());
            c.gemini_api_key = Some("kg".into());
        });
        assert_eq!(Llm::from_config(&no_anthropic, None).unwrap().describe(), "openai:gpt-6-luna");
        // A chosen provider answers alone; chosen but not configured = no model (never another provider).
        for (want, label) in [(BotProvider::Gemini, "gemini:gemini-3.5-flash-lite"), (BotProvider::Ollama, "ollama:llama3.2")] {
            let c = BrainConfig { provider: want, ..all.clone() };
            assert_eq!(Llm::from_config(&c, None).unwrap().describe(), label);
        }
        assert!(Llm::from_config(&BrainConfig { provider: BotProvider::Anthropic, ..no_anthropic.clone() }, None).is_none());
        // Blank model = the provider's default; keys never in Debug.
        let c = cfg_with(|c| {
            c.provider = BotProvider::Openai;
            c.openai_api_key = Some("sk-secret".into());
            c.openai_model = " ".into();
        });
        let llm = Llm::from_config(&c, None).unwrap();
        assert_eq!(llm.describe(), "openai:gpt-6-luna");
        assert!(!format!("{llm:?} {c:?}").contains("sk-secret"));
        // The hub's agent session comes last in Auto (with no keys it answers), and only when the hub gives one.
        let acp: Arc<dyn ModelSession> = Arc::new(Canned("Woof!"));
        assert_eq!(Llm::from_config(&BrainConfig::default(), Some(&acp)).unwrap().describe(), "acp:fake:sonnet");
        assert_eq!(Llm::from_config(&no_anthropic, Some(&acp)).unwrap().describe(), "openai:gpt-6-luna");
        let only_acp = BrainConfig { provider: BotProvider::Acp, ..all.clone() };
        assert_eq!(Llm::from_config(&only_acp, Some(&acp)).unwrap().describe(), "acp:fake:sonnet");
        assert!(Llm::from_config(&only_acp, None).is_none(), "acp chosen, agent not installed: no model");
    }

    /// A [`ModelSession`] with a canned reply; a JSON reply only when the instructions carry the schema.
    struct Canned(&'static str);

    impl ModelSession for Canned {
        fn label(&self) -> String {
            "fake:sonnet".into()
        }
        fn complete<'a>(&'a self, system: &'a str, _user: &'a str, _timeout: Duration) -> ModelFuture<'a> {
            let reply = self.0.to_string();
            let ok = !reply.starts_with('{') || system.contains("JSON schema");
            Box::pin(async move { if ok { Ok(reply) } else { Err(anyhow!("no schema in the instructions")) } })
        }
    }

    #[tokio::test]
    async fn acp_sessions_get_the_schema_in_the_instructions() {
        let llm = Llm::with_provider(Provider::Acp(Arc::new(Canned("{\"kind\":\"help\"}")))).unwrap();
        assert_eq!(llm.complete_json("sys", "hi", &schema(), 50).await.unwrap(), json!({ "kind": "help" }));
        let llm = Llm::with_provider(Provider::Acp(Arc::new(Canned("Woof!")))).unwrap();
        assert_eq!(llm.complete("sys", "hi", 50).await.unwrap(), "Woof!");
    }

    fn schema() -> Value {
        json!({ "type": "object", "properties": { "kind": { "type": "string" } }, "required": ["kind"], "additionalProperties": false })
    }

    #[test]
    fn request_bodies_follow_each_api() {
        let s = schema();
        let a = anthropic_body("m", "sys", "hi", 100, Some(&s));
        assert_eq!(a["output_config"]["format"], json!({ "type": "json_schema", "schema": s }));
        assert_eq!((a["system"].as_str(), a["messages"][0]["content"].as_str()), (Some("sys"), Some("hi")));
        assert!(anthropic_body("m", "sys", "hi", 100, None).get("output_config").is_none());
        let o = openai_body("gpt-6-luna", "sys", "hi", 100, Some(&s));
        assert_eq!(o["response_format"]["type"], "json_schema");
        assert_eq!(o["response_format"]["json_schema"], json!({ "name": SCHEMA_NAME, "strict": true, "schema": s }));
        assert_eq!((o["messages"][0]["role"].as_str(), o["messages"][1]["content"].as_str()), (Some("system"), Some("hi")));
        assert!(o["max_completion_tokens"].as_u64().unwrap() >= 400, "headroom for reasoning tokens");
        assert!(o.get("max_tokens").is_none(), "deprecated in favour of max_completion_tokens");
        assert_eq!(o["reasoning_effort"], "none", "the lowest effort GPT-6 lists");
        assert!(openai_body("gpt-4o-mini", "s", "u", 10, None).get("reasoning_effort").is_none());
        assert_eq!(openai_reasoning_effort("gpt-5-mini"), Some("minimal"));
        let g = gemini_body("gemini-3.5-flash-lite", "sys", "hi", 100, Some(&s));
        assert_eq!(g["generationConfig"]["thinkingConfig"], json!({ "thinkingLevel": "MINIMAL" }));
        assert_eq!(gemini_thinking("gemini-2.5-flash-lite"), Some(json!({ "thinkingBudget": 0 })));
        assert_eq!((gemini_thinking("gemini-2.5-pro"), gemini_thinking("gemma-3")), (None, None));
        assert_eq!(gemini_thinking("gemini-4-neon"), Some(json!({ "thinkingLevel": "MINIMAL" })));
        assert_eq!(g["systemInstruction"]["parts"][0]["text"], "sys");
        assert_eq!(g["contents"][0], json!({ "role": "user", "parts": [{ "text": "hi" }] }));
        assert_eq!(g["generationConfig"]["responseMimeType"], "application/json");
        assert_eq!(g["generationConfig"]["responseJsonSchema"], s);
        assert!(gemini_body("gemini-3.5-flash-lite", "sys", "hi", 100, None)["generationConfig"].get("responseMimeType").is_none());
        assert_eq!(ollama_body("llama3.2", "sys", "hi", 100, Some(&s))["format"], s);
        assert!(path_safe("gemini-3.5-flash-lite") && !path_safe("x/../../v1/files") && !path_safe("a?key=b"));
    }

    #[test]
    fn replies_parse_from_canned_json() {
        let openai = json!({ "choices": [{ "index": 0, "message": { "role": "assistant", "content": "{\"kind\":\"help\"}", "refusal": null }, "finish_reason": "stop" }] });
        assert_eq!(parse_json_reply(&openai_text(&openai).unwrap()).unwrap(), json!({ "kind": "help" }));
        let refused = json!({ "choices": [{ "message": { "role": "assistant", "content": null, "refusal": "I can't help with that." } }] });
        assert!(openai_text(&refused).is_err());
        let gemini = json!({ "candidates": [{ "content": { "role": "model", "parts": [{ "text": "thinking…", "thought": true }, { "text": "{\"kind\":" }, { "text": "\"status\"}" }] } }] });
        assert_eq!(parse_json_reply(&gemini_text(&gemini).unwrap()).unwrap(), json!({ "kind": "status" }));
        assert!(gemini_text(&json!({ "promptFeedback": { "blockReason": "SAFETY" } })).is_err());
        assert!(gemini_text(&json!({})).is_err());
        let anthropic = json!({ "content": [{ "type": "text", "text": "```json\n{\"kind\":\"smalltalk\"}\n```" }] });
        assert_eq!(parse_json_reply(&anthropic_text(&anthropic)).unwrap(), json!({ "kind": "smalltalk" }));
        assert!(parse_json_reply("not json").is_err() && parse_json_reply("[1]").is_err());
        assert_eq!(api_error(&json!({ "error": { "code": 400, "message": "API key not valid", "status": "INVALID_ARGUMENT" } }), ""), "API key not valid");
    }

    /// End to end over a local fake server (no network): OpenAI and Gemini shapes, auth headers.
    #[tokio::test]
    async fn fake_servers_answer_openai_and_gemini_calls() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        async fn serve(reply: &'static str) -> (String, tokio::task::JoinHandle<String>) {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            let h = tokio::spawn(async move {
                let (mut s, _) = l.accept().await.unwrap();
                let mut buf = vec![0u8; 65536];
                let mut got = Vec::new();
                loop {
                    let n = s.read(&mut buf).await.unwrap();
                    got.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&got).to_string();
                    if let Some(i) = text.find("\r\n\r\n") {
                        let len = text[..i]
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                            .unwrap_or(0);
                        if got.len() >= i + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len());
                s.write_all(resp.as_bytes()).await.unwrap();
                String::from_utf8_lossy(&got).to_string()
            });
            (format!("http://{addr}"), h)
        }
        let (url, h) = serve(r#"{"choices":[{"message":{"role":"assistant","content":"{\"kind\":\"help\"}","refusal":null}}]}"#).await;
        let llm = Llm::with_provider(Provider::OpenAi { api_key: "sk-test".into(), model: "gpt-6-luna".into(), url: format!("{url}/v1/chat/completions") }).unwrap();
        assert_eq!(llm.complete_json("sys", "hi", &schema(), 50).await.unwrap(), json!({ "kind": "help" }));
        let req = h.await.unwrap();
        assert!(req.starts_with("POST /v1/chat/completions") && req.to_ascii_lowercase().contains("authorization: bearer sk-test"), "{req}");
        let (url, h) = serve(r#"{"candidates":[{"content":{"parts":[{"text":"All quiet."}]}}]}"#).await;
        let llm = Llm::with_provider(Provider::Gemini { api_key: "g-test".into(), model: "gemini-3.5-flash-lite".into(), base: format!("{url}/v1beta/models") }).unwrap();
        assert_eq!(llm.complete("sys", "hi", 50).await.unwrap(), "All quiet.");
        let req = h.await.unwrap();
        assert!(req.starts_with("POST /v1beta/models/gemini-3.5-flash-lite:generateContent"), "{req}");
        assert!(req.to_ascii_lowercase().contains("x-goog-api-key: g-test"), "{req}");
        // A body larger than the cap is refused (declared or streamed).
        let big: &'static str = Box::leak(format!("{{\"choices\":[{{\"message\":{{\"content\":\"{}\"}}}}]}}", "x".repeat(MAX_RESPONSE_BYTES + 10)).into_boxed_str());
        let (url, h) = serve(big).await;
        let llm = Llm::with_provider(Provider::OpenAi { api_key: "k".into(), model: "gpt-6-luna".into(), url: format!("{url}/v1/chat/completions") }).unwrap();
        let err = llm.complete("sys", "hi", 50).await.unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err:#}");
        let _ = h.await;
    }
}
