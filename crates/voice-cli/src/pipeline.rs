//! The one-shot desktop pipeline: speech-to-text, Grok reasoning, text-to-speech.
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use voice_core::xai::{
    rest::{TranscriptWord, TtsRequest, TTS_SAMPLE_RATE},
    XaiRest,
};

pub const SYSTEM_PROMPT: &str = "You are the voice orchestration brain for a Windows desktop MVP.
Return strict JSON with:
- transcript: the user's utterance, normalized only lightly if needed
- intent: a short snake_case intent label
- response: a concise spoken reply that sounds natural aloud

Keep response helpful, brief, and safe for operator testing.
";

/// UTC timestamp like `2026-11-23T01:02:03.123456+00:00`.
pub fn utc_now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptEvent {
    pub request_id: String,
    pub timestamp: String,
    pub transcript: String,
    pub language: String,
    pub duration_seconds: f64,
    pub provider: String,
    pub words: Vec<TranscriptWord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReasoningResult {
    pub transcript: String,
    pub intent: String,
    pub response: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SynthesisEvent {
    pub request_id: String,
    pub timestamp: String,
    pub provider: String,
    pub voice_id: String,
    pub codec: String,
    pub sample_rate: u32,
    pub file_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InteractionLog {
    pub request_id: String,
    pub transcript_event: TranscriptEvent,
    pub reasoning_result: ReasoningResult,
    pub synthesis_event: SynthesisEvent,
}

fn reasoning_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "transcript": {"type": "string", "description": "Verbatim or lightly normalized user utterance."},
            "intent": {"type": "string", "description": "Short machine-readable intent label such as greeting or ask_status."},
            "response": {"type": "string", "description": "Natural language assistant response to speak back to the user."}
        },
        "required": ["transcript", "intent", "response"],
        "additionalProperties": false
    })
}

pub async fn transcribe_file(xai: &XaiRest, audio_path: &Path) -> anyhow::Result<TranscriptEvent> {
    let transcript = xai.transcribe_file(audio_path).await?;
    Ok(TranscriptEvent {
        request_id: uuid::Uuid::new_v4().to_string(),
        timestamp: utc_now_iso(),
        transcript: transcript.text,
        language: transcript.language,
        duration_seconds: transcript.duration,
        provider: "xai-stt".into(),
        words: transcript.words,
    })
}

pub async fn reason_over_transcript(
    xai: &XaiRest,
    transcript: &str,
) -> anyhow::Result<ReasoningResult> {
    let messages = vec![
        json!({"role": "system", "content": SYSTEM_PROMPT}),
        json!({"role": "user", "content": transcript}),
    ];
    let content = xai
        .chat_schema(messages, "ReasoningResult", reasoning_schema())
        .await?;
    serde_json::from_str(&content).context("Grok did not return {transcript, intent, response}")
}

pub async fn synthesize_to_wav(
    xai: &XaiRest,
    request_id: &str,
    text: &str,
    output_path: &Path,
) -> anyhow::Result<SynthesisEvent> {
    let request = TtsRequest {
        text,
        voice_id: "",
        language: "",
        text_normalization: true,
        speed: None,
    };
    let wav = xai.synthesize_wav(&request).await?;
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(output_path, wav).await?;
    let voice = xai.settings().tts_voice.trim();
    Ok(SynthesisEvent {
        request_id: request_id.to_string(),
        timestamp: utc_now_iso(),
        provider: "xai-tts".into(),
        voice_id: if voice.is_empty() {
            "eve".into()
        } else {
            voice.to_string()
        },
        codec: "wav".into(),
        sample_rate: TTS_SAMPLE_RATE,
        file_path: output_path.to_string_lossy().into_owned(),
    })
}

/// Append one JSON object per line (ASCII-escaped, like the original log).
pub fn append_jsonl(path: &Path, payload: &impl Serialize) -> anyhow::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(payload)?;
    let ascii: String = line
        .chars()
        .flat_map(|c| {
            if c.is_ascii() {
                vec![c]
            } else {
                let mut units = [0u16; 2];
                c.encode_utf16(&mut units)
                    .iter()
                    .flat_map(|u| format!("\\u{u:04x}").chars().collect::<Vec<_>>())
                    .collect()
            }
        })
        .collect();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{ascii}")?;
    Ok(())
}

/// Speech-to-text, reasoning and text-to-speech for an already recorded utterance.
pub async fn process_recording(
    xai: &XaiRest,
    capture_path: &Path,
    output_path: &Path,
) -> anyhow::Result<InteractionLog> {
    let transcript_event = transcribe_file(xai, capture_path).await?;
    let reasoning_result = reason_over_transcript(xai, &transcript_event.transcript).await?;
    let synthesis_event = synthesize_to_wav(
        xai,
        &transcript_event.request_id,
        &reasoning_result.response,
        output_path,
    )
    .await?;
    Ok(InteractionLog {
        request_id: transcript_event.request_id.clone(),
        transcript_event,
        reasoning_result,
        synthesis_event,
    })
}

/// File-name stamp for one interaction, UTC.
pub fn stamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

pub fn artifact_paths(artifacts_dir: &Path, stamp: &str) -> (PathBuf, PathBuf) {
    (
        artifacts_dir.join(format!("{stamp}-input.wav")),
        artifacts_dir.join(format!("{stamp}-output.wav")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use voice_core::config::XaiSettings;
    use wiremock::{
        matchers::{body_partial_json, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn client(server: &MockServer) -> XaiRest {
        XaiRest::new(XaiSettings {
            api_key: "k".into(),
            base_url: server.uri(),
            realtime_url: String::new(),
            grok_model: "grok-test".into(),
            tts_voice: "eve".into(),
            language: "en".into(),
            stt_keyterm: None,
        })
    }

    #[tokio::test]
    async fn full_pipeline_against_mock_xai() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/stt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "text": "what time is it", "language": "en", "duration": 1.2,
                "words": [{"text": "what", "start": 0.0, "end": 0.2}]
            })))
            .mount(&server)
            .await;
        let reply = json!({"transcript": "What time is it?", "intent": "ask_time", "response": "It is noon."});
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(
                json!({"response_format": {"type": "json_schema"}}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"choices": [{"message": {"content": reply.to_string()}}]}),
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tts"))
            .and(body_partial_json(
                json!({"text": "It is noon.", "voice_id": "eve", "text_normalization": true}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"RIFFwav".to_vec()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let (input, output) = artifact_paths(dir.path(), "20261123-010203");
        std::fs::write(&input, b"RIFF").unwrap();
        let log = process_recording(&client(&server), &input, &output)
            .await
            .unwrap();

        assert_eq!(log.transcript_event.transcript, "what time is it");
        assert_eq!(log.transcript_event.provider, "xai-stt");
        assert_eq!(log.reasoning_result.intent, "ask_time");
        assert_eq!(log.synthesis_event.request_id, log.request_id);
        assert_eq!(log.synthesis_event.sample_rate, 24000);
        assert_eq!(std::fs::read(&output).unwrap(), b"RIFFwav");
        assert!(output.ends_with("20261123-010203-output.wav"));

        let value = serde_json::to_value(&log).unwrap();
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "request_id",
                "transcript_event",
                "reasoning_result",
                "synthesis_event"
            ]
        );
    }

    #[test]
    fn jsonl_appends_ascii_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("interactions.jsonl");
        append_jsonl(&path, &json!({"a": "caf\u{e9}"})).unwrap();
        append_jsonl(&path, &json!({"b": 2})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "{\"a\":\"caf\\u00e9\"}\n{\"b\":2}\n");
    }
}
