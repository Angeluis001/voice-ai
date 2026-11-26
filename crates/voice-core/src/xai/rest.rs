//! xAI REST API: chat completions in JSON mode, speech-to-text, text-to-speech and
//! the voice catalogue. All calls use `Authorization: Bearer <XAI_API_KEY>`.
use crate::config::XaiSettings;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

#[derive(Debug, thiserror::Error)]
pub enum XaiError {
    #[error("XAI_API_KEY is missing. Add it to your local environment or .env file.")]
    MissingKey,
    #[error("xAI {status}: {message}")]
    Api { status: u16, message: String },
    #[error("xAI request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("xAI returned an unexpected response: {0}")]
    Shape(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl XaiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            XaiError::Api { status, .. } => Some(*status),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    pub speaker: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    pub language: String,
    pub duration: f64,
    pub words: Vec<TranscriptWord>,
}

#[derive(Debug, Clone, Default)]
pub struct TtsRequest<'a> {
    pub text: &'a str,
    pub voice_id: &'a str,
    pub language: &'a str,
    pub text_normalization: bool,
    pub speed: Option<f64>,
}

pub const TTS_SAMPLE_RATE: u32 = 24000;

#[derive(Clone)]
pub struct XaiRest {
    http: reqwest::Client,
    settings: XaiSettings,
}

/// Pull a readable message out of an xAI error body.
fn error_text(status: u16, body: &str) -> String {
    let clip = |s: &str| s.chars().take(800).collect::<String>();
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(map)) => {
            match map.get("error") {
                Some(Value::Object(err)) => {
                    for key in ["message", "code"] {
                        if let Some(Value::String(s)) = err.get(key) {
                            if !s.is_empty() {
                                return s.clone();
                            }
                        }
                    }
                    return Value::Object(err.clone()).to_string();
                }
                Some(Value::String(s)) if !s.is_empty() => return s.clone(),
                _ => {}
            }
            if let Some(Value::String(s)) = map.get("message") {
                if !s.is_empty() {
                    return s.clone();
                }
            }
            clip(&Value::Object(map).to_string())
        }
        Ok(other) => clip(&other.to_string()),
        Err(_) => {
            let text = body.trim();
            if text.is_empty() {
                format!("HTTP {status}")
            } else {
                clip(text)
            }
        }
    }
}

async fn check(response: reqwest::Response) -> Result<reqwest::Response, XaiError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    Err(XaiError::Api {
        status,
        message: error_text(status, &body),
    })
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("ogg") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("aac") => "audio/aac",
        _ => "application/octet-stream",
    }
}

impl XaiRest {
    pub fn new(settings: XaiSettings) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .expect("http client");
        Self { http, settings }
    }

    pub fn settings(&self) -> &XaiSettings {
        &self.settings
    }

    pub fn has_key(&self) -> bool {
        !self.settings.api_key.is_empty()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.settings.base_url, path)
    }

    fn key(&self) -> Result<&str, XaiError> {
        if self.has_key() {
            Ok(&self.settings.api_key)
        } else {
            Err(XaiError::MissingKey)
        }
    }

    /// Chat completion in JSON mode. Returns the assistant message content (a JSON string).
    pub async fn chat_json(&self, messages: Vec<Value>) -> Result<String, XaiError> {
        self.chat(json!({
            "model": self.settings.grok_model,
            "response_format": {"type": "json_object"},
            "messages": messages,
        }))
        .await
    }

    /// Chat completion constrained to a JSON schema (structured output).
    pub async fn chat_schema(
        &self,
        messages: Vec<Value>,
        name: &str,
        schema: Value,
    ) -> Result<String, XaiError> {
        self.chat(json!({
            "model": self.settings.grok_model,
            "response_format": {"type": "json_schema", "json_schema": {"name": name, "schema": schema, "strict": true}},
            "messages": messages,
        }))
        .await
    }

    async fn chat(&self, body: Value) -> Result<String, XaiError> {
        let response = self
            .http
            .post(self.url("/chat/completions"))
            .bearer_auth(self.key()?)
            .timeout(Duration::from_secs(120))
            .json(&body)
            .send()
            .await?;
        let payload: Value = check(response).await?.json().await?;
        payload["choices"][0]["message"]["content"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .ok_or_else(|| XaiError::Shape("chat completion had no content".into()))
    }

    /// Speech-to-text for an audio file.
    pub async fn transcribe_file(&self, path: &Path) -> Result<Transcript, XaiError> {
        let bytes = tokio::fs::read(path).await?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio")
            .to_string();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(name)
            .mime_str(content_type_for(path))
            .expect("static mime type");
        let mut form = reqwest::multipart::Form::new()
            .text("format", "true")
            .text("language", self.settings.language.clone());
        if let Some(keyterm) = &self.settings.stt_keyterm {
            form = form.text("keyterm", keyterm.clone());
        }
        let response = self
            .http
            .post(self.url("/stt"))
            .bearer_auth(self.key()?)
            .timeout(Duration::from_secs(300))
            .multipart(form.part("file", part))
            .send()
            .await?;
        let payload: Value = check(response).await?.json().await?;
        let text = payload["text"]
            .as_str()
            .ok_or_else(|| XaiError::Shape("speech-to-text response had no text".into()))?;
        let words = payload["words"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|w| serde_json::from_value(w.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Transcript {
            text: text.to_string(),
            language: payload["language"]
                .as_str()
                .unwrap_or(&self.settings.language)
                .to_string(),
            duration: payload["duration"].as_f64().unwrap_or(0.0),
            words,
        })
    }

    /// Text-to-speech. Returns WAV bytes at 24 kHz.
    pub async fn synthesize_wav(&self, request: &TtsRequest<'_>) -> Result<Vec<u8>, XaiError> {
        let voice = Some(request.voice_id.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or(&self.settings.tts_voice);
        let voice = if voice.is_empty() { "eve" } else { voice };
        let language = Some(request.language.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or(&self.settings.language);
        let language = if language.is_empty() { "en" } else { language };
        let mut body = json!({
            "text": request.text,
            "voice_id": voice,
            "language": language,
            "text_normalization": request.text_normalization,
            "output_format": {"codec": "wav", "sample_rate": TTS_SAMPLE_RATE},
        });
        if let Some(speed) = request.speed {
            if !(0.7..=1.5).contains(&speed) {
                return Err(XaiError::Shape(
                    "TTS speed must be between 0.7 and 1.5".into(),
                ));
            }
            body["speed"] = json!(speed);
        }
        let response = self
            .http
            .post(self.url("/tts"))
            .bearer_auth(self.key()?)
            .timeout(Duration::from_secs(120))
            .json(&body)
            .send()
            .await?;
        Ok(check(response).await?.bytes().await?.to_vec())
    }

    async fn list(&self, path: &str) -> Result<Vec<Value>, XaiError> {
        let response = self
            .http
            .get(self.url(path))
            .bearer_auth(self.key()?)
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        let payload: Value = check(response).await?.json().await?;
        let voices = match payload {
            Value::Object(mut map) => map.remove("voices").unwrap_or(Value::Null),
            other => other,
        };
        Ok(voices.as_array().cloned().unwrap_or_default())
    }

    pub async fn list_tts_voices(&self) -> Result<Vec<Value>, XaiError> {
        self.list("/tts/voices").await
    }

    pub async fn list_custom_voices(&self) -> Result<Vec<Value>, XaiError> {
        self.list("/custom-voices").await
    }

    /// Create a cloned voice from a reference sample. `fields` are extra form fields
    /// (name, language, use_case, gender, tone, description); empty values are skipped.
    pub async fn create_custom_voice(
        &self,
        fields: &[(&str, &str)],
        audio: Vec<u8>,
        filename: &str,
        content_type: &str,
    ) -> Result<Value, XaiError> {
        let mut form = reqwest::multipart::Form::new();
        for (key, value) in fields {
            if !value.is_empty() {
                form = form.text(key.to_string(), value.to_string());
            }
        }
        let filename = if filename.is_empty() {
            "reference.wav"
        } else {
            filename
        };
        let mime = if content_type.is_empty() {
            "application/octet-stream"
        } else {
            content_type
        };
        let part = reqwest::multipart::Part::bytes(audio)
            .file_name(filename.to_string())
            .mime_str(mime)
            .map_err(|e| XaiError::Shape(e.to_string()))?;
        let response = self
            .http
            .post(self.url("/custom-voices"))
            .bearer_auth(self.key()?)
            .timeout(Duration::from_secs(180))
            .multipart(form.part("file", part))
            .send()
            .await?;
        let payload: Value = check(response).await?.json().await?;
        if payload.is_object() {
            Ok(payload)
        } else {
            Err(XaiError::Shape(
                "xAI custom voice response was not an object".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{body_partial_json, header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn client(server: &MockServer, key: &str) -> XaiRest {
        XaiRest::new(XaiSettings {
            api_key: key.into(),
            base_url: server.uri(),
            realtime_url: String::new(),
            grok_model: "grok-test".into(),
            tts_voice: "eve".into(),
            language: "en".into(),
            stt_keyterm: None,
        })
    }

    #[tokio::test]
    async fn chat_json_sends_model_and_json_mode() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer k"))
            .and(body_partial_json(
                json!({"model": "grok-test", "response_format": {"type": "json_object"}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"choices": [{"message": {"content": "{\"intent\":\"ask_fees\"}"}}]}),
            ))
            .mount(&server)
            .await;
        let content = client(&server, "k")
            .chat_json(vec![json!({"role": "user", "content": "hi"})])
            .await
            .unwrap();
        assert_eq!(content, "{\"intent\":\"ask_fees\"}");
    }

    #[tokio::test]
    async fn api_errors_carry_status_and_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_json(json!({"error": {"message": "out of credit"}})),
            )
            .mount(&server)
            .await;
        let err = client(&server, "k").chat_json(vec![]).await.unwrap_err();
        assert_eq!(err.to_string(), "xAI 429: out of credit");
        assert_eq!(err.status(), Some(429));
    }

    #[tokio::test]
    async fn missing_key_fails_before_any_request() {
        let server = MockServer::start().await;
        let err = client(&server, "").chat_json(vec![]).await.unwrap_err();
        assert!(matches!(err, XaiError::MissingKey));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn tts_posts_wav_request_and_returns_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/tts"))
            .and(body_partial_json(json!({
                "text": "hello", "voice_id": "eve", "language": "en-AU", "speed": 0.9,
                "output_format": {"codec": "wav", "sample_rate": 24000}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"RIFFdata".to_vec()))
            .mount(&server)
            .await;
        let request = TtsRequest {
            text: "hello",
            voice_id: "",
            language: "en-AU",
            text_normalization: true,
            speed: Some(0.9),
        };
        assert_eq!(
            client(&server, "k").synthesize_wav(&request).await.unwrap(),
            b"RIFFdata"
        );
        let too_fast = TtsRequest {
            speed: Some(2.0),
            ..request
        };
        assert!(client(&server, "k")
            .synthesize_wav(&too_fast)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn stt_parses_text_and_words() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/stt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "text": "hello there", "language": "en", "duration": 1.5,
                "words": [{"text": "hello", "start": 0.0, "end": 0.4, "speaker": 0}, {"text": "there", "start": 0.5, "end": 0.9}]
            })))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        std::fs::write(&wav, b"RIFF").unwrap();
        let t = client(&server, "k").transcribe_file(&wav).await.unwrap();
        assert_eq!(t.text, "hello there");
        assert_eq!(t.words.len(), 2);
        assert_eq!(t.words[1].speaker, None);
    }

    #[tokio::test]
    async fn voice_lists_accept_wrapped_or_bare_arrays() {
        let server = MockServer::start().await;
        Mock::given(path("/tts/voices"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"voices": [{"voice_id": "eve"}]})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/custom-voices"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([{"voice_id": "c1"}, {"voice_id": "c2"}])),
            )
            .mount(&server)
            .await;
        let c = client(&server, "k");
        assert_eq!(c.list_tts_voices().await.unwrap().len(), 1);
        assert_eq!(c.list_custom_voices().await.unwrap().len(), 2);
    }

    #[test]
    fn error_text_shapes() {
        assert_eq!(error_text(500, ""), "HTTP 500");
        assert_eq!(error_text(400, "{\"error\":\"bad\"}"), "bad");
        assert_eq!(error_text(400, "{\"message\":\"nope\"}"), "nope");
        assert_eq!(error_text(400, "plain text"), "plain text");
    }
}
