//! xAI realtime voice WebSocket (Grok Voice Agent).
//!
//! The connection is exposed as two channels: JSON events to send, and JSON events
//! received. Audio travels inside events as base64 PCM16.
use crate::config::VadSettings;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

#[derive(Debug, Clone)]
pub struct RealtimeConfig {
    pub url: String,
    pub api_key: String,
    pub voice: String,
    pub instructions: String,
    pub sample_rate: u32,
    pub vad: VadSettings,
}

#[derive(Debug)]
pub enum RealtimeEvent {
    /// A JSON event from xAI.
    Event(Value),
    /// The socket closed normally.
    Closed,
    /// The socket failed.
    Error(String),
}

pub struct RealtimeConn {
    /// Client events to send to xAI.
    pub tx: mpsc::Sender<Value>,
    /// Server events from xAI.
    pub rx: mpsc::Receiver<RealtimeEvent>,
}

#[derive(Debug, thiserror::Error)]
pub enum RealtimeError {
    #[error("XAI_API_KEY is not configured")]
    MissingKey,
    #[error("xAI realtime connect timeout")]
    Timeout,
    #[error("xAI realtime connect failed: {0}")]
    Connect(String),
}

/// The `session.update` event that configures voice, instructions, turn detection
/// and audio formats.
pub fn session_update(cfg: &RealtimeConfig) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "voice": cfg.voice,
            "instructions": cfg.instructions,
            // Faster first reply on phone lines.
            "reasoning": {"effort": "none"},
            "turn_detection": {
                "type": "server_vad",
                "threshold": cfg.vad.threshold,
                "silence_duration_ms": cfg.vad.silence_ms,
                "prefix_padding_ms": cfg.vad.prefix_ms
            },
            "audio": {
                "input": {
                    "format": {"type": "audio/pcm", "rate": cfg.sample_rate},
                    // Live captions need a transcription model.
                    "transcription": {
                        "model": cfg.vad.transcribe_model,
                        "language_hint": cfg.vad.language_hint
                    }
                },
                "output": {"format": {"type": "audio/pcm", "rate": cfg.sample_rate}}
            }
        }
    })
}

/// The greeting is spoken verbatim as a forced assistant message.
pub fn force_message(text: &str) -> Value {
    json!({
        "type": "conversation.item.create",
        "item": {
            "type": "force_message",
            "role": "assistant",
            "interruptible": true,
            "content": [{"type": "output_text", "text": text}]
        }
    })
}

pub fn audio_append(pcm_base64: String) -> Value {
    json!({"type": "input_audio_buffer.append", "audio": pcm_base64})
}

pub fn response_cancel() -> Value {
    json!({"type": "response.cancel"})
}

/// A refused handshake carries xAI's reason in the response body (bad key, bad model).
fn handshake_error(error: tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Http(response) => {
            let status = response.status();
            let body = response
                .body()
                .as_deref()
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            // The body may still carry chunked-transfer framing around the JSON.
            let body = match (body.find('{'), body.rfind('}')) {
                (Some(start), Some(end)) if end > start => body[start..=end].to_string(),
                _ => body.trim().to_string(),
            };
            let detail = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v["error"]
                        .as_str()
                        .or(v["error"]["message"].as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| body.chars().take(300).collect());
            if detail.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}: {detail}")
            }
        }
        other => other.to_string(),
    }
}

/// Connect, configure the session and wait (briefly) for xAI to acknowledge it.
pub async fn connect(cfg: &RealtimeConfig) -> Result<RealtimeConn, RealtimeError> {
    if cfg.api_key.is_empty() {
        return Err(RealtimeError::MissingKey);
    }
    let mut request = cfg
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| RealtimeError::Connect(e.to_string()))?;
    let bearer = format!("Bearer {}", cfg.api_key)
        .parse()
        .map_err(|_| RealtimeError::Connect("API key is not a valid header value".into()))?;
    request.headers_mut().insert("Authorization", bearer);

    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| RealtimeError::Timeout)?
    .map_err(|e| RealtimeError::Connect(handshake_error(e)))?;
    let (mut sink, mut stream) = socket.split();

    sink.send(Message::text(session_update(cfg).to_string()))
        .await
        .map_err(|e| RealtimeError::Connect(e.to_string()))?;

    let (event_tx, event_rx) = mpsc::channel::<RealtimeEvent>(1024);
    let (client_tx, mut client_rx) = mpsc::channel::<Value>(1024);

    // Wait up to 1.5 s for the session acknowledgement; carry on regardless.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
    loop {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Err(_) => break,
            Ok(None) => return Err(RealtimeError::Connect("socket closed during setup".into())),
            Ok(Some(Err(e))) => return Err(RealtimeError::Connect(e.to_string())),
            Ok(Some(Ok(Message::Text(text)))) => {
                let Ok(event) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                let kind = event["type"].as_str().unwrap_or_default();
                if kind == "session.updated" || kind == "session.created" {
                    tracing::info!(phase = "xai_session", kind, rate = cfg.sample_rate);
                    break;
                }
                let _ = event_tx.send(RealtimeEvent::Event(event)).await;
            }
            Ok(Some(Ok(_))) => {}
        }
    }

    tokio::spawn(async move {
        while let Some(event) = client_rx.recv().await {
            if sink.send(Message::text(event.to_string())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    tokio::spawn(async move {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(event) = serde_json::from_str::<Value>(text.as_str()) {
                        if event_tx.send(RealtimeEvent::Event(event)).await.is_err() {
                            break;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    let _ = event_tx.send(RealtimeEvent::Closed).await;
                    break;
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    let _ = event_tx.send(RealtimeEvent::Error(e.to_string())).await;
                    break;
                }
            }
        }
    });

    Ok(RealtimeConn {
        tx: client_tx,
        rx: event_rx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RealtimeConfig {
        RealtimeConfig {
            url: "wss://example/realtime".into(),
            api_key: "k".into(),
            voice: "eve".into(),
            instructions: "Be brief.".into(),
            sample_rate: 8000,
            vad: VadSettings::default(),
        }
    }

    #[test]
    fn session_update_matches_the_working_bridge() {
        assert_eq!(
            session_update(&cfg()),
            json!({"type": "session.update", "session": {
                "voice": "eve",
                "instructions": "Be brief.",
                "reasoning": {"effort": "none"},
                "turn_detection": {"type": "server_vad", "threshold": 0.55, "silence_duration_ms": 480, "prefix_padding_ms": 200},
                "audio": {
                    "input": {"format": {"type": "audio/pcm", "rate": 8000},
                              "transcription": {"model": "grok-transcribe", "language_hint": "en"}},
                    "output": {"format": {"type": "audio/pcm", "rate": 8000}}
                }
            }})
        );
    }

    #[test]
    fn greeting_is_a_forced_interruptible_message() {
        let event = force_message("Good morning");
        assert_eq!(event["item"]["type"], "force_message");
        assert_eq!(event["item"]["interruptible"], true);
        assert_eq!(
            event["item"]["content"][0],
            json!({"type": "output_text", "text": "Good morning"})
        );
    }

    #[tokio::test]
    async fn missing_key_is_rejected_without_connecting() {
        let mut c = cfg();
        c.api_key.clear();
        assert!(matches!(connect(&c).await, Err(RealtimeError::MissingKey)));
    }
}
