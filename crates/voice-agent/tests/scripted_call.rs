//! A whole softphone call with no network and no API key: a fake xAI realtime
//! server, a fake FreeSWITCH event socket and a fake audio fork play a scripted
//! conversation against the real agent.
//!
//! Script: greeting -> caller interrupts (barge-in) -> caller asks for a person ->
//! AI confirms -> intent says transfer -> call is transferred -> fork closes.
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use voice_agent::serve::{router, ServeState};
use voice_core::{
    call::CallDeps,
    config::{AgentSettings, EslSettings, VadSettings, XaiSettings},
    esl,
    session::SessionStore,
};
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

const GREETING_PCM: [i16; 4] = [1000, 2000, 3000, 4000];
const REPLY_PCM: [i16; 2] = [-500, -600];

fn pcm(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// What the fake xAI server saw from the agent.
#[derive(Default)]
struct XaiLog {
    events: Vec<Value>,
}

/// Fake xAI realtime endpoint. Reacts to the agent's events like the real service.
async fn start_fake_xai(log: Arc<Mutex<XaiLog>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/realtime", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut heard_caller = false;
        while let Some(Ok(message)) = ws.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            let event: Value = serde_json::from_str(text.as_str()).unwrap();
            let kind = event["type"].as_str().unwrap_or_default().to_string();
            log.lock().unwrap().events.push(event.clone());
            let mut replies: Vec<Value> = vec![];
            match kind.as_str() {
                "session.update" => replies.push(json!({"type": "session.updated"})),
                "conversation.item.create" => {
                    let greeting = event["item"]["content"][0]["text"]
                        .as_str()
                        .unwrap()
                        .to_string();
                    replies.push(json!({"type": "response.created"}));
                    replies.push(json!({"type": "response.output_audio.delta", "delta": BASE64.encode(pcm(&GREETING_PCM))}));
                    replies.push(json!({"type": "response.output_audio_transcript.done", "transcript": greeting}));
                    replies.push(json!({"type": "response.done"}));
                }
                "input_audio_buffer.append" if !heard_caller => {
                    heard_caller = true;
                    // The caller starts talking while the greeting is still audible.
                    ws.send(Message::text(
                        json!({"type": "input_audio_buffer.speech_started"}).to_string(),
                    ))
                    .await
                    .unwrap();
                    tokio::time::sleep(Duration::from_millis(700)).await;
                    replies.push(
                        json!({"type": "conversation.item.input_audio_transcription.completed",
                                        "transcript": "Can I speak to a person please"}),
                    );
                    replies.push(json!({"type": "response.created"}));
                    replies.push(json!({"type": "response.output_audio.delta", "delta": BASE64.encode(pcm(&REPLY_PCM))}));
                    replies.push(json!({"type": "response.output_audio_transcript.delta", "delta": "Of course, "}));
                    replies.push(json!({"type": "response.output_audio_transcript.done",
                                        "transcript": "Of course, I'll transfer you now."}));
                    replies.push(json!({"type": "response.done"}));
                }
                _ => {}
            }
            for reply in replies {
                ws.send(Message::text(reply.to_string())).await.unwrap();
            }
        }
    });
    url
}

#[tokio::test]
async fn scripted_softphone_call_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let xai_log = Arc::new(Mutex::new(XaiLog::default()));
    let realtime_url = start_fake_xai(xai_log.clone()).await;
    let fake_esl = esl::fake::start("secret").await;

    // Chat completions: used for intent labelling and for post-call analysis.
    let chat = MockServer::start().await;
    let label = json!({
        "intent": "request_transfer",
        "slots": {"action": "transfer", "staff_name": null},
        "primary_intent": "request_transfer",
        "summary": "Caller asked for a person and was transferred.",
        "outcome": "transferred"
    });
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": label.to_string()}}]})),
        )
        .mount(&chat)
        .await;

    let settings = AgentSettings {
        xai: XaiSettings {
            api_key: "test-key".into(),
            base_url: chat.uri(),
            realtime_url,
            grok_model: "grok-test".into(),
            tts_voice: "eve".into(),
            language: "en".into(),
            stt_keyterm: None,
        },
        vad: VadSettings::default(),
        esl: EslSettings {
            host: "127.0.0.1".into(),
            port: fake_esl.port,
            password: "secret".into(),
        },
        listen_port: 0,
        result_dir: dir.path().join("results"),
        media_dir: dir.path().join("media"),
        sample_rate: 8000,
        local_sample_rate: 24000,
        input_device: None,
        output_device: None,
        clients_dir: dir.path().join("no-clients"),
        default_client_id: "default".into(),
        sop_greeting: None,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agent_addr = listener.local_addr().unwrap();
    let app = router(ServeState::new(CallDeps::new(settings)));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Health endpoint answers plain HTTP.
    let health = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(agent_addr).await.unwrap();
        s.write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut body = String::new();
        s.read_to_string(&mut body).await.unwrap();
        body
    };
    assert!(
        health.contains("\"service\":\"voice-ai-realtime-bridge\""),
        "{health}"
    );

    // FreeSWITCH connects its audio fork.
    let mut request = format!("ws://{agent_addr}/media?call_id=call-1&uuid=chan-1&target=9198")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        "audio.drachtio.org".parse().unwrap(),
    );
    let (mut fork, response) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(
        response.headers()["sec-websocket-protocol"],
        "audio.drachtio.org"
    );

    // The greeting audio arrives as one raw binary frame.
    let first = tokio::time::timeout(Duration::from_secs(5), fork.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.into_data().to_vec(), pcm(&GREETING_PCM));

    // The caller speaks. A JSON control frame must be ignored, not treated as audio.
    fork.send(Message::text("{\"type\":\"metadata\"}".to_string()))
        .await
        .unwrap();
    fork.send(Message::binary(pcm(&[100, 100, 100, 100])))
        .await
        .unwrap();

    // The AI's reply reaches the caller.
    let reply = tokio::time::timeout(Duration::from_secs(5), fork.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reply.into_data().to_vec(), pcm(&REPLY_PCM));

    // The transfer happens about 2.5 s after the turn is labelled.
    let transferred = async {
        loop {
            if fake_esl
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.starts_with("uuid_transfer"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(8), transferred)
        .await
        .expect("call was transferred");

    // FreeSWITCH closes the fork when the call is bridged away.
    fork.close(None).await.unwrap();
    let session_path = dir.path().join("results").join("call-1-session.json");
    let finished = async {
        loop {
            if let Some(doc) = SessionStore::load(&session_path).await {
                if doc
                    .get("post_analysis")
                    .and_then(|a| a.get("status"))
                    .is_some()
                {
                    return doc;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    let doc = tokio::time::timeout(Duration::from_secs(8), finished)
        .await
        .expect("post-call analysis written");

    // --- Session document ---
    assert_eq!(doc["conversation_id"], "call-1");
    assert_eq!(doc["channel_uuid"], "chan-1");
    assert_eq!(doc["status"], "transferred");
    assert_eq!(doc["mode"], "realtime");
    assert_eq!(doc["latest_phase"], "completed");
    assert_eq!(doc["client_company"], "Val");
    assert_eq!(doc["slots"]["action"], "transfer");
    let turns = doc["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "greeting + one caller turn: {turns:#?}");
    assert_eq!(turns[0]["turn"], 0);
    assert_eq!(turns[0]["intent"], "opening_greeting");
    assert_eq!(
        turns[0]["response"],
        "Good morning, how can I help you today?"
    );
    assert_eq!(turns[0]["request_id"], "call-1-greeting");
    assert_eq!(turns[1]["turn"], 1);
    assert_eq!(turns[1]["transcript"], "Can I speak to a person please");
    assert_eq!(turns[1]["response"], "Of course, I'll transfer you now.");
    assert_eq!(turns[1]["intent"], "request_transfer");
    assert_eq!(turns[1]["action"], "transfer");
    assert_eq!(turns[1]["phase"], "completed");
    assert_eq!(turns[1]["status"], "completed");
    assert_eq!(turns[1]["request_id"], "call-1-rt1");
    let brief = doc["handoff_brief"].as_str().unwrap();
    assert!(brief.starts_with(
        "INTERNAL HANDOFF BRIEF\nCall: call-1\nIntent: request_transfer\nAction: transfer"
    ));
    assert!(brief.contains("Caller last said: Can I speak to a person please"));
    assert_eq!(doc["post_analysis"]["status"], "ready");
    assert_eq!(doc["post_analysis"]["outcome"], "transferred");
    assert_eq!(
        doc["post_analysis"]["summary"],
        "Caller asked for a person and was transferred."
    );
    assert_eq!(doc["latest_intent"], "request_transfer");

    // --- Recording: caller and AI mixed into one WAV ---
    let wav = std::fs::read(dir.path().join("media").join("chan-1-call.wav")).unwrap();
    assert_eq!(&wav[0..4], b"RIFF");
    let samples: Vec<i16> = wav[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    // Caller frame (4 x 100) mixed with the queued greeting, then leftover AI audio.
    assert_eq!(&samples[..4], &[1025, 1975, 2925, 3875]);
    assert_eq!(&samples[4..], &[-475, -570]);
    assert!(doc["recording_path"]
        .as_str()
        .unwrap()
        .ends_with("media/chan-1-call.wav"));

    // --- What xAI was sent ---
    let events = xai_log.lock().unwrap().events.clone();
    let kinds: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(&kinds[..2], ["session.update", "conversation.item.create"]);
    assert_eq!(
        events[0]["session"]["audio"]["input"]["format"],
        json!({"type": "audio/pcm", "rate": 8000})
    );
    assert_eq!(events[0]["session"]["voice"], "eve");
    assert!(events[0]["session"]["instructions"]
        .as_str()
        .unwrap()
        .contains("first-line phone receptionist"));
    let appends: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "input_audio_buffer.append")
        .collect();
    assert_eq!(
        appends.len(),
        1,
        "one caller frame, the JSON control frame is not audio"
    );
    assert_eq!(
        appends[0]["audio"],
        BASE64.encode(pcm(&[100, 100, 100, 100]))
    );
    assert_eq!(
        kinds.iter().filter(|k| **k == "response.cancel").count(),
        2,
        "barge-in + transfer: {kinds:?}"
    );
    assert!(
        !kinds.contains(&"response.create"),
        "replies come from server turn detection"
    );

    // --- What FreeSWITCH was told ---
    let commands = fake_esl.commands.lock().unwrap().clone();
    assert_eq!(
        commands,
        vec![
            "uuid_audio_fork chan-1 stop_play", // barge-in
            "uuid_audio_fork chan-1 stop_play", // before transfer
            "uuid_transfer chan-1 1000 XML mrf",
        ]
    );
}
