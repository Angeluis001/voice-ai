//! Brand voice messages for ops: create, clone, preview, download.
//!
//! Port of `voice_in_agent/brand.py`. Per-client branded phone announcements are
//! synthesised through xAI text-to-speech and kept in a library on disk:
//!
//! ```text
//! {brand_dir}/{client_id}/voices.json            catalog of registered cloned voices
//! {brand_dir}/{client_id}/messages/{id}.json     message metadata
//! {brand_dir}/{client_id}/messages/{id}.wav      synthesised audio
//! ```
use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
use voice_core::xai::{rest::TtsRequest, XaiError, XaiRest};

#[cfg(test)]
mod tests;

/// Message kinds: key, label.
pub const MESSAGE_KINDS: [(&str, &str); 5] = [
    ("closed", "Closed period"),
    ("after_hours", "After hours"),
    ("holiday", "Holiday"),
    ("greeting", "Greeting"),
    ("custom", "Custom"),
];

/// Stock voices offered when xAI cannot be reached: voice_id, name, description.
pub const FALLBACK_STOCK_VOICES: [(&str, &str, &str); 5] = [
    ("eve", "Eve", "Default brand voice"),
    ("ara", "Ara", "Warm, clear"),
    ("rex", "Rex", "Direct, confident"),
    ("leo", "Leo", "Calm, professional"),
    ("sal", "Sal", "Friendly narrator"),
];

pub const SPEED_MIN: f64 = 0.7;
pub const SPEED_MAX: f64 = 1.5;
pub const SPEED_DEFAULT: f64 = 1.0;

/// Accents/languages accepted by xAI TTS: code, label.
pub const TTS_LANGUAGES: [(&str, &str); 24] = [
    ("en", "English"),
    ("en-GB", "English — UK"),
    ("en-AU", "English — Australia"),
    ("en-US", "English — United States"),
    ("es-ES", "Spanish — Spain"),
    ("es-MX", "Spanish — Mexico"),
    ("pt-PT", "Portuguese — Portugal"),
    ("pt-BR", "Portuguese — Brazil"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("ar-AE", "Arabic — UAE"),
    ("ar-SA", "Arabic — Saudi Arabia"),
    ("ar-EG", "Arabic — Egypt"),
    ("zh", "Chinese"),
    ("hi", "Hindi"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("id", "Indonesian"),
    ("tr", "Turkish"),
    ("vi", "Vietnamese"),
    ("ru", "Russian"),
    ("bn", "Bengali"),
    ("auto", "Auto-detect"),
];

const AUDIO_SUFFIXES: [&str; 7] = [".wav", ".mp3", ".m4a", ".flac", ".ogg", ".aac", ".opus"];
const MAX_SAMPLE_BYTES: usize = 25 * 1024 * 1024;
const MAX_SCRIPT_CHARS: usize = 15000;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn safe_id() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$").expect("static regex"))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BrandError {
    pub message: String,
    pub status_code: u16,
    pub code: String,
}

impl BrandError {
    /// A plain validation error: 400 / `brand_error`.
    pub fn new(message: impl Into<String>) -> Self {
        Self::with(message, 400, "brand_error")
    }

    pub fn with(message: impl Into<String>, status_code: u16, code: &str) -> Self {
        Self {
            message: message.into(),
            status_code,
            code: code.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

fn d_eve() -> String {
    "eve".into()
}
fn d_en() -> String {
    "en".into()
}
fn d_stock() -> String {
    "stock".into()
}
fn d_custom() -> String {
    "custom".into()
}
fn d_speed() -> f64 {
    SPEED_DEFAULT
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrandClient {
    pub id: String,
    pub display_name: String,
    pub company: String,
    #[serde(default = "d_eve")]
    pub tts_voice: String,
    #[serde(default = "d_en")]
    pub language: String,
    #[serde(default)]
    pub greeting: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrandVoice {
    pub voice_id: String,
    pub name: String,
    /// `stock` or `cloned`.
    #[serde(default = "d_stock")]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "d_en")]
    pub language: String,
    /// `xai`, `fallback` or `catalog`.
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrandMessage {
    pub id: String,
    pub client_id: String,
    pub title: String,
    /// `closed`, `after_hours`, `holiday`, `greeting` or `custom`.
    #[serde(default = "d_custom")]
    pub kind: String,
    pub script: String,
    pub voice_id: String,
    #[serde(default)]
    pub voice_name: String,
    #[serde(default = "d_en")]
    pub language: String,
    #[serde(default = "d_speed")]
    pub speed: f64,
    #[serde(default)]
    pub starts_on: String,
    #[serde(default)]
    pub ends_on: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub audio_bytes: u64,
    #[serde(default)]
    pub download_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VoiceList {
    pub client: BrandClient,
    pub pack_voice_id: String,
    pub stock: Vec<BrandVoice>,
    pub cloned: Vec<BrandVoice>,
    pub xai_error: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SynthesizeRequest {
    pub title: String,
    pub kind: String,
    pub script: String,
    pub voice_id: String,
    pub voice_name: String,
    pub language: String,
    pub speed: f64,
    pub starts_on: String,
    pub ends_on: String,
}

impl Default for SynthesizeRequest {
    fn default() -> Self {
        Self {
            title: String::new(),
            kind: "custom".into(),
            script: String::new(),
            voice_id: String::new(),
            voice_name: String::new(),
            language: String::new(),
            speed: SPEED_DEFAULT,
            starts_on: String::new(),
            ends_on: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CloneRequest {
    pub name: String,
    pub gender: String,
    pub tone: String,
    pub language: String,
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BrandSettings {
    pub brand_dir: PathBuf,
    pub clients_dir: PathBuf,
    pub default_client_id: String,
}

// ---------------------------------------------------------------------------
// xAI seam
// ---------------------------------------------------------------------------

/// The xAI calls the brand library needs. Implemented for [`XaiRest`]; tests pass a fake.
#[async_trait]
pub trait BrandXai: Send + Sync {
    async fn synthesize_wav(&self, request: &TtsRequest<'_>) -> Result<Vec<u8>, XaiError>;
    async fn list_tts_voices(&self) -> Result<Vec<Value>, XaiError>;
    async fn list_custom_voices(&self) -> Result<Vec<Value>, XaiError>;
    async fn create_custom_voice(
        &self,
        fields: &[(&str, &str)],
        audio: Vec<u8>,
        filename: &str,
        content_type: &str,
    ) -> Result<Value, XaiError>;
}

#[async_trait]
impl BrandXai for XaiRest {
    async fn synthesize_wav(&self, request: &TtsRequest<'_>) -> Result<Vec<u8>, XaiError> {
        XaiRest::synthesize_wav(self, request).await
    }
    async fn list_tts_voices(&self) -> Result<Vec<Value>, XaiError> {
        XaiRest::list_tts_voices(self).await
    }
    async fn list_custom_voices(&self) -> Result<Vec<Value>, XaiError> {
        XaiRest::list_custom_voices(self).await
    }
    async fn create_custom_voice(
        &self,
        fields: &[(&str, &str)],
        audio: Vec<u8>,
        filename: &str,
        content_type: &str,
    ) -> Result<Value, XaiError> {
        XaiRest::create_custom_voice(self, fields, audio, filename, content_type).await
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

pub fn require_id(value: &str, label: &str) -> Result<String, BrandError> {
    let text = value.trim();
    if !safe_id().is_match(text) {
        return Err(BrandError::new(format!("Invalid {label}")));
    }
    Ok(text.to_string())
}

fn check_speed(speed: f64) -> Result<f64, BrandError> {
    if speed.is_nan() {
        return Err(BrandError::new("Speed must be a number"));
    }
    if !(SPEED_MIN..=SPEED_MAX).contains(&speed) {
        return Err(BrandError::new(format!(
            "Speed must be between {SPEED_MIN} and {SPEED_MAX}"
        )));
    }
    Ok((speed * 100.0).round() / 100.0)
}

/// `None`, JSON null and `""` give the default 1.0; numbers and numeric strings are
/// range-checked and rounded to two decimals.
pub fn normalize_speed(value: Option<&Value>) -> Result<f64, BrandError> {
    let not_a_number = || BrandError::new("Speed must be a number");
    let speed = match value {
        None | Some(Value::Null) => return Ok(SPEED_DEFAULT),
        Some(Value::String(s)) if s.is_empty() => return Ok(SPEED_DEFAULT),
        Some(Value::String(s)) => s.trim().parse::<f64>().map_err(|_| not_a_number())?,
        Some(Value::Number(n)) => n.as_f64().ok_or_else(not_a_number)?,
        Some(_) => return Err(not_a_number()),
    };
    check_speed(speed)
}

pub fn normalize_language(value: &str, fallback: &str) -> Result<String, BrandError> {
    let raw = [value.trim(), fallback.trim()]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("en");
    let lower = raw.to_lowercase();
    TTS_LANGUAGES
        .iter()
        .find(|(code, _)| code.to_lowercase() == lower)
        .map(|(code, _)| code.to_string())
        .ok_or_else(|| BrandError::new(format!("Unknown accent/language '{raw}'")))
}

pub fn language_label(code: &str) -> String {
    match TTS_LANGUAGES.iter().find(|(c, _)| *c == code) {
        Some((_, label)) => label.to_string(),
        None if code.is_empty() => "English".to_string(),
        None => code.to_string(),
    }
}

/// Label for a message kind key (exact match), e.g. `closed` -> `Closed period`.
pub fn kind_label(kind: &str) -> Option<&'static str> {
    MESSAGE_KINDS
        .iter()
        .find(|(key, _)| *key == kind)
        .map(|(_, label)| *label)
}

/// Title the console suggests next to a script template (the `/template` route).
pub fn template_title(kind: &str, starts_on: &str, ends_on: &str) -> String {
    let title = kind_label(&kind.to_lowercase()).unwrap_or("Brand message");
    if !starts_on.is_empty() && !ends_on.is_empty() {
        format!("{title} {starts_on} to {ends_on}")
    } else {
        title.to_string()
    }
}

/// The stock script for a message kind. Unknown kinds (and `custom`) give `""`.
pub fn script_template(
    kind: &str,
    company: &str,
    starts_on: &str,
    ends_on: &str,
    greeting: &str,
) -> String {
    let name = Some(company.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("us");
    let start = pretty_date(starts_on);
    let end = pretty_date(ends_on);
    let both = !start.is_empty() && !end.is_empty();
    match kind.trim().to_lowercase().as_str() {
        "closed" if both => format!(
            "Thank you for calling {name}. We are closed from {start} to {end}. \
             Please call back when we reopen, or leave a message after the tone."
        ),
        "closed" => format!(
            "Thank you for calling {name}. We are closed for a short period. \
             Please call back when we reopen, or leave a message after the tone."
        ),
        "holiday" if both => format!(
            "Thank you for calling {name}. We are closed for the holiday period \
             from {start} to {end}. We look forward to speaking with you when we reopen."
        ),
        "holiday" => format!(
            "Thank you for calling {name}. We are closed for the holiday period. \
             Please call back when we reopen."
        ),
        "after_hours" => format!(
            "Thank you for calling {name}. Our office is closed for the day. \
             Please call back during business hours, or leave a message after the tone."
        ),
        "greeting" if !greeting.trim().is_empty() => greeting.trim().to_string(),
        "greeting" => {
            format!("Good morning, thank you for calling {name}. How can I help you today?")
        }
        _ => String::new(),
    }
}

pub fn download_filename(message: &BrandMessage) -> String {
    if !message.download_name.is_empty() {
        return message.download_name.clone();
    }
    let source = [&message.title, &message.kind]
        .into_iter()
        .find(|s| !s.is_empty())
        .map_or("message", |s| s.as_str());
    format!(
        "{}-{}-{}.wav",
        message.client_id,
        message.kind,
        slug(source, "message")
    )
}

fn slug(text: &str, fallback: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"[^a-zA-Z0-9]+").expect("static regex"));
    let cleaned = re
        .replace_all(text.trim(), "-")
        .trim_matches('-')
        .to_lowercase();
    let cut: String = cleaned.chars().take(48).collect();
    if cut.is_empty() {
        fallback.to_string()
    } else {
        cut
    }
}

/// Parse the way Python's `strptime(raw[:10], "%Y-%m-%d")` does: (year, month, day).
fn parse_ymd(raw: &str) -> Option<(i32, u32, u32)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^([0-9]{4})-([0-9]{1,2})-([0-9]{1,2})$").expect("static regex")
    });
    let head: String = raw.chars().take(10).collect();
    let caps = re.captures(&head)?;
    let year: i32 = caps[1].parse().ok()?;
    let month: u32 = caps[2].parse().ok()?;
    let day: u32 = caps[3].parse().ok()?;
    if year < 1 {
        return None;
    }
    chrono::NaiveDate::from_ymd_opt(year, month, day).map(|_| (year, month, day))
}

fn pretty_date(value: &str) -> String {
    let raw = value.trim();
    match parse_ymd(raw) {
        Some((year, month, day)) => format!("{day} {} {year}", MONTHS[(month - 1) as usize]),
        None => raw.to_string(),
    }
}

fn date_or_blank(value: &str) -> Result<String, BrandError> {
    let raw = value.trim();
    if raw.is_empty() {
        return Ok(String::new());
    }
    if parse_ymd(raw).is_none() {
        return Err(BrandError::new("Dates must be YYYY-MM-DD"));
    }
    Ok(raw.chars().take(10).collect())
}

fn default_title(kind: &str, starts_on: &str, ends_on: &str) -> String {
    let label = kind_label(kind).unwrap_or("Brand message");
    if !starts_on.is_empty() && !ends_on.is_empty() {
        format!("{label} {starts_on} to {ends_on}")
    } else {
        label.to_string()
    }
}

/// Same shape as Python's `datetime.now(UTC).isoformat()`.
fn utc_now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

/// Python's `str(value or "")`: `None` for falsy values, otherwise the text.
fn py_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null | Value::Bool(false) => None,
        Value::Bool(true) => Some("True".to_string()),
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) if n.as_f64() == Some(0.0) => None,
        Value::Number(n) => Some(n.to_string()),
        Value::Array(a) if a.is_empty() => None,
        Value::Object(o) if o.is_empty() => None,
        other => Some(other.to_string()),
    }
}

/// First truthy value among `candidates`, as text; `""` when none.
fn first_text(candidates: &[Option<&Value>]) -> String {
    candidates
        .iter()
        .find_map(|v| py_text(*v))
        .unwrap_or_default()
}

/// Pretty JSON the way Python's `json.dumps(obj, indent=2)` writes it (ASCII only).
fn py_json_pretty<T: Serialize>(value: &T) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string());
    let mut out = String::with_capacity(pretty.len());
    let mut units = [0u16; 2];
    for ch in pretty.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            for unit in ch.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// Lower-cased extension including the dot, like `Path(name).suffix.lower()`.
fn file_suffix(filename: &str) -> String {
    let name = filename.rsplit(['/', '\\']).next().unwrap_or("");
    match name.rfind('.') {
        Some(idx) if idx > 0 && idx + 1 < name.len() => name[idx..].to_lowercase(),
        _ => String::new(),
    }
}

fn voice_from_xai(raw: &Value, kind: &str, source: &str) -> Option<BrandVoice> {
    let raw = raw.as_object()?;
    let voice_id = first_text(&[raw.get("voice_id"), raw.get("id")])
        .trim()
        .to_string();
    if voice_id.is_empty() {
        return None;
    }
    let name = py_text(raw.get("name"))
        .unwrap_or_else(|| voice_id.clone())
        .trim()
        .to_string();
    let description = first_text(&[raw.get("description"), raw.get("tone"), raw.get("use_case")])
        .trim()
        .to_string();
    Some(BrandVoice {
        voice_id,
        name,
        kind: kind.to_string(),
        description,
        language: py_text(raw.get("language")).unwrap_or_else(d_en),
        source: source.to_string(),
    })
}

fn xai_unavailable(err: &XaiError) -> Option<BrandError> {
    matches!(err, XaiError::MissingKey)
        .then(|| BrandError::with(err.to_string(), 503, "xai_unavailable"))
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct BrandStore {
    settings: Arc<BrandSettings>,
    xai: Arc<dyn BrandXai>,
}

impl BrandStore {
    pub fn new(settings: BrandSettings, xai: Arc<dyn BrandXai>) -> Self {
        Self {
            settings: Arc::new(settings),
            xai,
        }
    }

    pub fn settings(&self) -> &BrandSettings {
        &self.settings
    }

    /// Client packs found under `clients_dir` (folders starting with `_` are skipped).
    pub fn list_clients(&self) -> Vec<BrandClient> {
        let Ok(entries) = std::fs::read_dir(&self.settings.clients_dir) else {
            return Vec::new();
        };
        let mut folders: Vec<(String, PathBuf)> = entries
            .flatten()
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.path(),
                )
            })
            .collect();
        folders.sort_by_key(|(name, _)| name.to_lowercase());

        let mut clients = Vec::new();
        for (folder_name, folder) in folders {
            if !folder.is_dir() || folder_name.starts_with('_') {
                continue;
            }
            let pack_path = folder.join("client.json");
            if !pack_path.is_file() {
                continue;
            }
            let Some(pack) = read_json(&pack_path) else {
                continue;
            };
            let Some(pack) = pack.as_object() else {
                continue;
            };
            let empty = Map::new();
            let brand = pack
                .get("brand")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let client_id = py_text(pack.get("id"))
                .unwrap_or(folder_name)
                .trim()
                .to_string();
            if !safe_id().is_match(&client_id) {
                continue;
            }
            let or_id = |candidates: &[Option<&Value>]| {
                candidates
                    .iter()
                    .find_map(|v| py_text(*v))
                    .unwrap_or_else(|| client_id.clone())
            };
            clients.push(BrandClient {
                display_name: or_id(&[pack.get("display_name"), brand.get("company")]),
                company: or_id(&[brand.get("company"), pack.get("display_name")]),
                tts_voice: py_text(brand.get("tts_voice")).unwrap_or_else(d_eve),
                language: py_text(brand.get("language")).unwrap_or_else(d_en),
                greeting: py_text(brand.get("greeting")).unwrap_or_default(),
                id: client_id.clone(),
            });
        }
        clients
    }

    pub fn get_client(&self, client_id: &str) -> Result<BrandClient, BrandError> {
        let wanted = require_id(client_id, "client id")?;
        self.list_clients()
            .into_iter()
            .find(|client| client.id == wanted)
            .ok_or_else(|| {
                BrandError::with(format!("Unknown client '{wanted}'"), 404, "unknown_client")
            })
    }

    /// `VOICE_AI_DEFAULT_CLIENT_ID` or the configured default when that pack exists,
    /// otherwise the first pack.
    pub fn default_client_id(&self) -> String {
        let wanted = std::env::var("VOICE_AI_DEFAULT_CLIENT_ID")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| self.settings.default_client_id.clone())
            .trim()
            .to_string();
        let clients = self.list_clients();
        if clients.is_empty() || clients.iter().any(|c| c.id == wanted) {
            return wanted;
        }
        clients[0].id.clone()
    }

    pub fn script_template(
        &self,
        client: &BrandClient,
        kind: &str,
        starts_on: &str,
        ends_on: &str,
    ) -> String {
        script_template(kind, &client.company, starts_on, ends_on, &client.greeting)
    }

    /// Stock and cloned voices for a client. xAI failures never fail the call: the first
    /// error is reported in `xai_error` and stock voices fall back to a built-in list.
    pub async fn list_voices(&self, client_id: &str) -> Result<VoiceList, BrandError> {
        let client = self.get_client(client_id)?;
        let mut stock: Vec<BrandVoice> = Vec::new();
        let mut cloned: Vec<BrandVoice> = Vec::new();
        let mut xai_error = String::new();

        match self.xai.list_tts_voices().await {
            Ok(raw) => stock.extend(raw.iter().filter_map(|v| voice_from_xai(v, "stock", "xai"))),
            Err(err) => {
                xai_error = err.to_string();
                tracing::warn!("Stock voice list failed: {err}");
            }
        }
        if stock.is_empty() {
            stock = FALLBACK_STOCK_VOICES
                .iter()
                .map(|(voice_id, name, description)| BrandVoice {
                    voice_id: voice_id.to_string(),
                    name: name.to_string(),
                    kind: d_stock(),
                    description: description.to_string(),
                    language: d_en(),
                    source: "fallback".to_string(),
                })
                .collect();
        }

        let mut seen: std::collections::HashSet<String> =
            stock.iter().map(|v| v.voice_id.to_lowercase()).collect();
        match self.xai.list_custom_voices().await {
            Ok(raw) => {
                for voice in raw
                    .iter()
                    .filter_map(|v| voice_from_xai(v, "cloned", "xai"))
                {
                    if seen.insert(voice.voice_id.to_lowercase()) {
                        cloned.push(voice);
                    }
                }
            }
            Err(err) => {
                if xai_error.is_empty() {
                    xai_error = err.to_string();
                }
                tracing::warn!("Custom voice list failed: {err}");
            }
        }

        for item in self.load_catalog(&client.id) {
            let voice_id = first_text(&[item.get("voice_id")]).trim().to_string();
            if voice_id.is_empty() || !seen.insert(voice_id.to_lowercase()) {
                continue;
            }
            cloned.push(BrandVoice {
                name: py_text(item.get("name")).unwrap_or_else(|| voice_id.clone()),
                kind: "cloned".to_string(),
                description: first_text(&[item.get("description")]),
                language: py_text(item.get("language"))
                    .or_else(|| Some(client.language.clone()).filter(|l| !l.is_empty()))
                    .unwrap_or_else(d_en),
                source: "catalog".to_string(),
                voice_id,
            });
        }

        Ok(VoiceList {
            pack_voice_id: client.tts_voice.clone(),
            client,
            stock,
            cloned,
            xai_error,
        })
    }

    /// Add (or replace) a cloned voice in the client's catalog by its xAI voice ID.
    pub fn register_cloned_voice(
        &self,
        client_id: &str,
        voice_id: &str,
        name: &str,
        description: &str,
        language: &str,
    ) -> Result<BrandVoice, BrandError> {
        let client = self.get_client(client_id)?;
        let vid = require_id(voice_id, "voice id")?;
        let name = (if name.is_empty() { vid.as_str() } else { name })
            .trim()
            .to_string();
        let description = description.trim().to_string();
        let language = [language, client.language.as_str()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("en")
            .trim()
            .to_string();

        let mut catalog = self.load_catalog(&client.id);
        catalog.retain(|item| {
            first_text(&[item.get("voice_id")]).to_lowercase() != vid.to_lowercase()
        });
        let entry = json!({
            "voice_id": vid,
            "name": name,
            "description": description,
            "language": language,
            "source": "imported",
            "created_at": utc_now_iso(),
        });
        if let Value::Object(entry) = entry {
            catalog.push(entry);
        }
        self.save_catalog(&client.id, &catalog)?;
        Ok(BrandVoice {
            voice_id: vid,
            name,
            kind: "cloned".to_string(),
            description,
            language,
            source: "catalog".to_string(),
        })
    }

    /// Clone a voice from an uploaded sample through xAI, then register it in the catalog.
    pub async fn clone_voice_from_sample(
        &self,
        client_id: &str,
        req: CloneRequest,
    ) -> Result<BrandVoice, BrandError> {
        let client = self.get_client(client_id)?;
        let label = req.name.trim().to_string();
        if label.is_empty() {
            return Err(BrandError::new("Give the cloned voice a name"));
        }
        if req.bytes.is_empty() {
            return Err(BrandError::new("Upload a voice sample"));
        }
        if req.bytes.len() > MAX_SAMPLE_BYTES {
            return Err(BrandError::new("Sample is too large (max 25 MB)"));
        }
        let suffix = file_suffix(if req.filename.is_empty() {
            "sample.wav"
        } else {
            &req.filename
        });
        if !suffix.is_empty() && !AUDIO_SUFFIXES.contains(&suffix.as_str()) {
            return Err(BrandError::new("Use a WAV, MP3, M4A, FLAC, or OGG sample"));
        }

        let language = [req.language.as_str(), client.language.as_str()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("en");
        let description = format!("Brand voice for {}", client.company);
        let fields = [
            ("name", label.as_str()),
            ("language", language),
            ("use_case", "narration"),
            ("gender", req.gender.as_str()),
            ("tone", req.tone.as_str()),
            ("description", description.as_str()),
        ];
        let filename = if req.filename.is_empty() {
            "reference.wav"
        } else {
            &req.filename
        };
        let content_type = if req.content_type.is_empty() {
            "audio/wav"
        } else {
            &req.content_type
        };
        let created = match self
            .xai
            .create_custom_voice(&fields, req.bytes, filename, content_type)
            .await
        {
            Ok(created) => created,
            Err(XaiError::Api {
                status: 403,
                message,
            }) => {
                return Err(BrandError::with(
                    format!(
                        "xAI clone-from-API is not enabled on this team (Enterprise). \
                         Clone the sample in the xAI voice library, then paste the Voice ID here. \
                         ({message})"
                    ),
                    403,
                    "clone_api_gated",
                ));
            }
            Err(err) => {
                return Err(xai_unavailable(&err)
                    .unwrap_or_else(|| BrandError::with(err.to_string(), 502, "clone_failed")));
            }
        };

        let voice_id = first_text(&[created.get("voice_id")]).trim().to_string();
        if voice_id.is_empty() {
            return Err(BrandError::with(
                "Clone succeeded but no voice_id was returned",
                502,
                "brand_error",
            ));
        }
        let name = py_text(created.get("name")).unwrap_or(label);
        let description = first_text(&[created.get("description")]);
        let language = py_text(created.get("language"))
            .or_else(|| Some(req.language.clone()).filter(|l| !l.is_empty()))
            .unwrap_or_else(|| client.language.clone());
        self.register_cloned_voice(&client.id, &voice_id, &name, &description, &language)
    }

    /// Saved messages, newest first. Unreadable or malformed files are skipped.
    pub fn list_messages(&self, client_id: &str) -> Result<Vec<BrandMessage>, BrandError> {
        let client = self.get_client(client_id)?;
        let folder = self.messages_dir(&client.id);
        let Ok(entries) = std::fs::read_dir(&folder) else {
            return Ok(Vec::new());
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();
        let mut items: Vec<BrandMessage> = paths
            .iter()
            .filter_map(|path| read_json(path))
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
        let key = |m: &BrandMessage| {
            if m.updated_at.is_empty() {
                m.created_at.clone()
            } else {
                m.updated_at.clone()
            }
        };
        items.sort_by_key(|m| std::cmp::Reverse(key(m)));
        Ok(items)
    }

    pub fn get_message(
        &self,
        client_id: &str,
        message_id: &str,
    ) -> Result<BrandMessage, BrandError> {
        let client = self.get_client(client_id)?;
        let mid = require_id(message_id, "message id")?;
        let path = self.messages_dir(&client.id).join(format!("{mid}.json"));
        if !path.is_file() {
            return Err(BrandError::with(
                "Message not found",
                404,
                "unknown_message",
            ));
        }
        read_json(&path)
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| {
                BrandError::with("Message file is unreadable", 500, "message_unreadable")
            })
    }

    /// Where the WAV for a message lives. The message must exist; the file may not.
    pub fn message_audio_path(
        &self,
        client_id: &str,
        message_id: &str,
    ) -> Result<PathBuf, BrandError> {
        self.get_message(client_id, message_id)?;
        let client = self.get_client(client_id)?;
        let mid = require_id(message_id, "message id")?;
        Ok(self.messages_dir(&client.id).join(format!("{mid}.wav")))
    }

    pub fn delete_message(&self, client_id: &str, message_id: &str) -> Result<(), BrandError> {
        let wav = self.message_audio_path(client_id, message_id)?;
        let _ = std::fs::remove_file(wav.with_extension("json"));
        let _ = std::fs::remove_file(wav);
        Ok(())
    }

    /// Validate, synthesise through xAI TTS and save the message and its WAV.
    pub async fn synthesize_message(
        &self,
        client_id: &str,
        req: SynthesizeRequest,
    ) -> Result<BrandMessage, BrandError> {
        let client = self.get_client(client_id)?;
        let text = req.script.trim().to_string();
        if text.is_empty() {
            return Err(BrandError::new("Write the message script first"));
        }
        if text.chars().count() > MAX_SCRIPT_CHARS {
            return Err(BrandError::new(
                "Script is too long (max 15,000 characters)",
            ));
        }
        let vid = [req.voice_id.as_str(), client.tts_voice.as_str()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("eve")
            .trim()
            .to_string();
        if vid.is_empty() {
            return Err(BrandError::new("Choose a voice"));
        }
        let key = (if req.kind.is_empty() {
            "custom"
        } else {
            req.kind.as_str()
        })
        .trim()
        .to_lowercase();
        if kind_label(&key).is_none() {
            return Err(BrandError::new("Unknown message type"));
        }
        let start = date_or_blank(&req.starts_on)?;
        let end = date_or_blank(&req.ends_on)?;
        if !start.is_empty() && !end.is_empty() && start > end {
            return Err(BrandError::new(
                "Closed-from date must be before closed-until",
            ));
        }

        let heading = Some(req.title.trim())
            .filter(|t| !t.is_empty())
            .map_or_else(|| default_title(&key, &start, &end), str::to_string);
        let message_id: String = uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(12)
            .collect();
        let lang = normalize_language(
            &req.language,
            if client.language.is_empty() {
                "en"
            } else {
                &client.language
            },
        )?;
        let rate = check_speed(req.speed)?;
        let now = utc_now_iso();
        let mut message = BrandMessage {
            id: message_id,
            client_id: client.id.clone(),
            title: heading,
            kind: key,
            script: text,
            voice_name: (if req.voice_name.is_empty() {
                vid.as_str()
            } else {
                req.voice_name.as_str()
            })
            .trim()
            .to_string(),
            voice_id: vid,
            language: lang,
            speed: rate,
            starts_on: start,
            ends_on: end,
            created_at: now.clone(),
            updated_at: now,
            audio_bytes: 0,
            download_name: String::new(),
        };

        let folder = self.messages_dir(&client.id);
        let wav_path = folder.join(format!("{}.wav", message.id));
        let request = TtsRequest {
            text: &message.script,
            voice_id: &message.voice_id,
            language: &message.language,
            text_normalization: true,
            speed: Some(rate),
        };
        let tts_failed = |text: String| BrandError::with(text, 502, "tts_failed");
        let audio = match self.xai.synthesize_wav(&request).await {
            Ok(audio) => audio,
            Err(err) => {
                let _ = std::fs::remove_file(&wav_path);
                return Err(xai_unavailable(&err).unwrap_or_else(|| tts_failed(err.to_string())));
            }
        };
        if let Err(err) = tokio::fs::write(&wav_path, &audio).await {
            let _ = std::fs::remove_file(&wav_path);
            return Err(tts_failed(err.to_string()));
        }

        message.audio_bytes = std::fs::metadata(&wav_path).map(|m| m.len()).unwrap_or(0);
        message.download_name = download_filename(&message);
        let json_path = folder.join(format!("{}.json", message.id));
        std::fs::write(&json_path, py_json_pretty(&message)).map_err(|err| {
            BrandError::with(
                format!("Could not save the message: {err}"),
                500,
                "storage_failed",
            )
        })?;
        Ok(message)
    }

    fn client_dir(&self, client_id: &str) -> PathBuf {
        let path = self.settings.brand_dir.join(client_id);
        let _ = std::fs::create_dir_all(&path);
        path
    }

    fn messages_dir(&self, client_id: &str) -> PathBuf {
        let path = self.client_dir(client_id).join("messages");
        let _ = std::fs::create_dir_all(&path);
        path
    }

    fn load_catalog(&self, client_id: &str) -> Vec<Map<String, Value>> {
        let path = self.client_dir(client_id).join("voices.json");
        let list = match read_json(&path) {
            Some(Value::Array(list)) => list,
            Some(Value::Object(mut map)) => match map.remove("voices") {
                Some(Value::Array(list)) => list,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        list.into_iter()
            .filter_map(|item| match item {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .collect()
    }

    fn save_catalog(
        &self,
        client_id: &str,
        catalog: &[Map<String, Value>],
    ) -> Result<(), BrandError> {
        let path = self.client_dir(client_id).join("voices.json");
        std::fs::write(&path, py_json_pretty(&json!({ "voices": catalog }))).map_err(|err| {
            BrandError::with(
                format!("Could not save the voice catalog: {err}"),
                500,
                "storage_failed",
            )
        })
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}
