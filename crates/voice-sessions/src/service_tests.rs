use super::*;
use crate::prompts::POST_ANALYSIS_PROMPT;
use std::time::Duration;
use tempfile::TempDir;
use voice_core::config::XaiSettings;
use wiremock::{
    matchers::{body_partial_json, method, path as url_path},
    Mock, MockServer, ResponseTemplate,
};

struct Fixture {
    dir: TempDir,
    service: SessionService,
}

impl Fixture {
    fn results(&self) -> PathBuf {
        self.dir.path().join("results")
    }
    fn media(&self) -> PathBuf {
        self.dir.path().join("media")
    }
    fn write(&self, name: &str, value: Value) -> PathBuf {
        let path = self.results().join(name);
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        path
    }
    /// Write a results file and make it look `age_secs` old.
    fn write_aged(&self, name: &str, value: Value, age_secs: u64) -> PathBuf {
        let path = self.write(name, value);
        set_age(&path, age_secs);
        path
    }
    fn recording(&self, name: &str, bytes: usize) -> PathBuf {
        let path = self.media().join(name);
        std::fs::write(&path, vec![0u8; bytes]).unwrap();
        path
    }
    fn sessions(&self) -> Vec<LiveCallSession> {
        self.service.list_live_call_sessions(50)
    }
    fn session(&self, id: &str) -> LiveCallSession {
        self.sessions()
            .into_iter()
            .find(|s| s.session_id == id)
            .unwrap_or_else(|| panic!("no session {id}"))
    }
}

fn set_age(path: &Path, age_secs: u64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
        .unwrap();
}

fn fixture_with(base_url: &str, api_key: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let settings = ConsoleSettings::for_dir(dir.path());
    std::fs::create_dir_all(&settings.telephony_results_dir).unwrap();
    std::fs::create_dir_all(&settings.telephony_media_dir).unwrap();
    let xai = XaiRest::new(XaiSettings {
        api_key: api_key.into(),
        base_url: base_url.into(),
        realtime_url: String::new(),
        grok_model: "grok-test".into(),
        tts_voice: "eve".into(),
        language: "en".into(),
        stt_keyterm: None,
    });
    Fixture {
        service: SessionService::new(settings, xai),
        dir,
    }
}

fn fixture() -> Fixture {
    fixture_with("http://127.0.0.1:9", "")
}

fn ago(secs: i64) -> String {
    (Utc::now() - chrono::Duration::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn turn_json(turn: i64, call_id: &str, transcript: &str, response: &str) -> Value {
    json!({"turn": turn, "call_id": call_id, "request_id": format!("r{turn}"), "transcript": transcript,
           "intent": "new_enquiry", "response": response, "phase": "completed", "status": "completed"})
}

fn result_turn(
    turn: Option<i64>,
    phase: &str,
    transcript: &str,
    response: &str,
    request_id: &str,
) -> VoiceInResult {
    let mut t = VoiceInResult::new("telephony", request_id, transcript, "", response);
    t.turn = turn;
    t.phase = phase.into();
    t
}

async fn chat_mock(server: &MockServer, content: Value) {
    Mock::given(method("POST"))
        .and(url_path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": content.to_string()}}]})),
        )
        .mount(server)
        .await;
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

#[test]
fn session_file_is_mapped_onto_the_model() {
    let f = fixture();
    let path = f.write_aged(
        "abc-session.json",
        json!({
            "conversation_id": "abc", "status": "completed", "target_number": "1000", "prompt_profile": "enquiries",
            "started_at": "2026-07-08T01:00:00.000Z", "updated_at": "2026-07-08T01:05:00.000Z",
            "slots": {"debt_type": "invoice", "action": "callback"}, "channel_uuid": "chan-1",
            "turns": [
                {"turn": 0, "call_id": "call-1", "request_id": "r0", "transcript": "", "intent": "opening_greeting", "response": "Welcome"},
                {"turn": "1", "call_id": "call-1", "request_id": "r1", "transcript": "I am owed money", "intent": "new_enquiry",
                 "response": "How much?", "slots": {"action": "continue"}, "timings_ms": {"total": "812"}}
            ],
            "post_analysis": {"summary": "Done", "primary_intent": "new_enquiry", "internal_handoff_brief": "brief"},
            "content": {"artifacts": [{"key": "k", "title": "t", "body": "b"}]}
        }),
        600,
    );
    let sessions = f.sessions();
    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s.session_id, "abc");
    assert_eq!(s.call_id.as_deref(), Some("call-1"));
    assert_eq!(s.source, "telephony");
    assert_eq!(s.status, "completed");
    assert_eq!(s.started_at, "2026-07-08T01:00:00.000Z");
    assert_eq!(s.updated_at, "2026-07-08T01:05:00.000Z");
    assert_eq!(s.latest_intent, "new_enquiry");
    assert_eq!(s.latest_transcript, "I am owed money");
    assert_eq!(s.latest_response, "How much?");
    assert_eq!(s.latest_action.as_deref(), Some("continue"));
    assert_eq!(s.target_number.as_deref(), Some("1000"));
    assert_eq!(s.prompt_profile.as_deref(), Some("enquiries"));
    assert_eq!(s.slots.get("debt_type"), Some(&json!("invoice")));
    assert_eq!(s.latest_latency_ms, Some(812));
    assert_eq!(s.latest_phase, "completed");
    assert_eq!(s.turn_count, 2);
    assert_eq!(s.mode, "realtime");
    assert!(!s.is_active);
    assert!(s.has_post_analysis);
    assert_eq!(s.post_analysis.as_ref().unwrap().summary, "Done");
    assert_eq!(s.handoff_brief, "brief");
    assert_eq!(
        s.content.as_ref().unwrap().artifacts[0].content_type,
        "text"
    );
    assert_eq!(s.channel_uuid, "chan-1");
    assert_eq!(
        s.recording_path,
        "/var/lib/voice-ai/pilot/media/chan-1-call.wav"
    );
    assert!(!s.has_recording);

    let t = &s.turns[1];
    assert_eq!(
        (
            t.turn,
            t.status.as_str(),
            t.phase.as_str(),
            t.provider.as_str()
        ),
        (Some(1), "completed", "completed", "xai")
    );
    assert_eq!(t.session_id.as_deref(), Some("abc"));
    assert_eq!(t.action.as_deref(), Some("continue"));
    assert_eq!(t.target_number.as_deref(), Some("1000"));
    assert_eq!(
        t.raw_result_path.as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
    let observed = parse_iso(t.observed_at.as_deref().unwrap()).unwrap();
    assert!((Utc::now() - observed).num_seconds() >= 590);
    assert_eq!(t.timestamp, t.observed_at.clone().unwrap());
    assert_eq!(s.turns[0].turn, Some(0));
}

#[test]
fn defaults_apply_when_the_file_is_sparse() {
    let f = fixture();
    f.write("bare-session.json", json!({"turns": [{"phase": "reasoned", "transcript": "hi"}], "to_user": "9199", "handoff_brief": "stored"}));
    let s = f.session("bare");
    assert_eq!(s.status, "active");
    assert_eq!(s.call_id.as_deref(), Some("bare"));
    assert_eq!(s.target_number.as_deref(), Some("9199"));
    assert_eq!(s.latest_action, None);
    assert_eq!(s.handoff_brief, "stored");
    assert_eq!(
        (
            s.turns[0].turn,
            s.turns[0].status.as_str(),
            s.turns[0].request_id.as_str()
        ),
        (Some(1), "processing", "unknown-request")
    );
    assert_eq!(s.latest_phase, "reasoned");
    // No timestamps in the file: the file's modification time is used.
    assert_eq!(s.started_at, s.updated_at);
    assert!(parse_iso(&s.updated_at).is_some());
    assert!(s.is_active);
    assert!(s.post_analysis.is_none() && s.content.is_none() && !s.has_post_analysis);
}

#[test]
fn unknown_conversation_id_falls_back_to_file_name_then_call_id() {
    let f = fixture();
    f.write(
        "f1-session.json",
        json!({"conversation_id": "unknown", "turns": [turn_json(1, "c9", "hello", "hi")]}),
    );
    f.write(
        "unknown-session.json",
        json!({"conversation_id": " unknown ", "turns": [turn_json(1, "unknown", "a", "b"), turn_json(2, "c7", "c", "d")]}),
    );
    let s = f.session("f1");
    assert_eq!(s.call_id.as_deref(), Some("c9"));
    assert_eq!(s.turns[0].session_id.as_deref(), Some("f1"));
    let s = f.session("c7");
    assert_eq!(s.turn_count, 2);
    assert_eq!(s.turns[0].session_id.as_deref(), Some("c7"));
}

#[test]
fn unusable_files_are_skipped_without_panicking() {
    let f = fixture();
    std::fs::write(f.results().join("broken-session.json"), "{not json").unwrap();
    std::fs::write(f.results().join("binary-session.json"), [0xff, 0xfe, 0x00]).unwrap();
    std::fs::write(f.results().join("broken-turn1.json"), "nope").unwrap();
    std::fs::create_dir(f.results().join("dir-session.json")).unwrap();
    f.write("list-session.json", json!([1, 2]));
    f.write("nullturns-session.json", json!({"turns": null}));
    f.write("badturn-session.json", json!({"turns": ["text"]}));
    f.write(
        "badtype-session.json",
        json!({"turns": [{"transcript": 5}]}),
    );
    f.write("badstatus-session.json", json!({"status": 7, "turns": []}));
    f.write(
        "ended-empty-session.json",
        json!({"status": "completed", "turns": []}),
    );
    f.write("badjob.json", json!({"job": "x"}));
    f.write("stored-junk-session.json", json!({"turns": [turn_json(1, "c", "hi", "yo")], "post_analysis": {"summary": 5}, "content": "x"}));
    f.write("empty-active-session.json", json!({"turns": []}));
    std::fs::write(
        f.results().join("bom-session.json"),
        "\u{feff}{\"turns\": []}",
    )
    .unwrap();

    let mut ids: Vec<String> = f.sessions().into_iter().map(|s| s.session_id).collect();
    ids.sort();
    assert_eq!(ids, ["bom", "empty-active", "stored-junk"]);
    let s = f.session("stored-junk");
    assert!(s.post_analysis.is_none() && s.content.is_none() && !s.has_post_analysis);
    assert!(f.service.list_recent_telephony_results(10).is_empty());
}

#[test]
fn telephony_result_file_is_normalised() {
    let payload = json!({
        "job": {"request_id": "req1", "call_id": "call-1", "conversation_id": "conv-job", "to_user": "1000", "turn": "2",
                "input_path": "/in.wav", "output_path": "/out.wav", "prompt_profile": "enquiries"},
        "reasoning": {"transcript": "I need help", "intent": "new_enquiry", "response": "Sure", "slots": {"action": "transfer"}},
        "transcript": {"text": "i need help", "language": "en", "duration": "2.5"},
        "provider": "xai", "status": "completed", "phase": "completed", "timings_ms": {"total": 900.7}
    });
    let r =
        normalize_telephony_payload(&payload, Some("p.json"), Some("2026-07-08T01:00:00+00:00"))
            .unwrap();
    assert_eq!(
        (
            r.source.as_str(),
            r.request_id.as_str(),
            r.call_id.as_deref()
        ),
        ("telephony", "req1", Some("call-1"))
    );
    assert_eq!(r.session_id.as_deref(), Some("conv-job"));
    assert_eq!(r.timestamp, "2026-07-08T01:00:00+00:00");
    assert_eq!(
        (
            r.transcript.as_str(),
            r.language.as_str(),
            r.duration_seconds
        ),
        ("I need help", "en", 2.5)
    );
    assert_eq!(
        (r.turn, r.action.as_deref(), r.target_number.as_deref()),
        (Some(2), Some("transfer"), Some("1000"))
    );
    assert_eq!(
        (
            r.input_audio_path.as_deref(),
            r.output_audio_path.as_deref()
        ),
        (Some("/in.wav"), Some("/out.wav"))
    );
    assert_eq!(latency_ms(r.timings_ms.as_ref()), Some(900));

    let sparse = normalize_telephony_payload(
        &json!({"reasoning": null, "turn": 3, "completed_at": "t"}),
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        (
            sparse.request_id.as_str(),
            sparse.provider.as_str(),
            sparse.timestamp.as_str()
        ),
        ("unknown-request", "unknown", "t")
    );
    assert_eq!(
        (sparse.session_id, sparse.turn, sparse.slots, sparse.action),
        (None, Some(3), None, None)
    );
    let fallback = normalize_telephony_payload(&json!({"job": {"call_id": "c"}, "reasoning": {"action": "callback"}, "transcript": {"text": "raw"}}), None, None).unwrap();
    assert_eq!(
        (
            fallback.session_id.as_deref(),
            fallback.action.as_deref(),
            fallback.transcript.as_str()
        ),
        (Some("c"), Some("callback"), "raw")
    );

    for bad in [
        json!([]),
        json!({"job": null}),
        json!({"transcript": "text"}),
        json!({"reasoning": "x"}),
        json!({"phase": null}),
    ] {
        assert!(
            normalize_telephony_payload(&bad, None, None).is_none(),
            "{bad}"
        );
    }
}

#[test]
fn sessions_are_rebuilt_from_results_when_no_session_file_exists() {
    let f = fixture();
    let result = |turn: i64, text: &str| {
        json!({"job": {"request_id": format!("q{turn}"), "call_id": "call-z", "to_user": "1000"}, "conversation_id": "conv-z", "turn": turn,
               "reasoning": {"transcript": text, "intent": "new_enquiry", "response": "ok"}, "status": "completed", "phase": "completed",
               "timings_ms": {"total": 700 + turn}})
    };
    f.write_aged("q2-turn2.json", result(2, "second"), 400);
    f.write_aged("q1-turn1.json", result(1, "first"), 500);
    let sessions = f.sessions();
    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(
        (s.session_id.as_str(), s.call_id.as_deref(), s.turn_count),
        ("conv-z", Some("call-z"), 2)
    );
    assert_eq!(
        (
            s.status.as_str(),
            s.latest_transcript.as_str(),
            s.latest_phase.as_str()
        ),
        ("completed", "second", "completed")
    );
    assert_eq!(s.latest_latency_ms, Some(702));
    assert_eq!(s.started_at, s.turns[0].observed_at.clone().unwrap());
    assert!(parse_iso(&s.started_at).unwrap() < parse_iso(&s.updated_at).unwrap());
    assert!(!s.is_active);
}

// ---------------------------------------------------------------------------
// Turn de-duplication
// ---------------------------------------------------------------------------

#[test]
fn dedupe_prefers_completed_then_later_richer_entries() {
    let out = dedupe_session_turns(vec![
        result_turn(Some(1), "completed", "a", "", "x"),
        result_turn(Some(1), "reasoned", "a much longer text", "", "y"),
    ]);
    assert_eq!((out.len(), out[0].request_id.as_str()), (1, "x"));
    let out = dedupe_session_turns(vec![
        result_turn(Some(1), "reasoned", "a much longer text", "", "y"),
        result_turn(Some(1), "completed", "a", "", "x"),
    ]);
    assert_eq!((out.len(), out[0].request_id.as_str()), (1, "x"));
    // Same rank: the later entry wins when it has at least as much text.
    let out = dedupe_session_turns(vec![
        result_turn(Some(1), "completed", "ab", "", "first"),
        result_turn(Some(1), "completed", "a", "b", "second"),
    ]);
    assert_eq!(out[0].request_id, "second");
    let out = dedupe_session_turns(vec![
        result_turn(Some(1), "completed", "abcd", "", "first"),
        result_turn(Some(1), "completed", "a", "", "second"),
    ]);
    assert_eq!(out[0].request_id, "first");
}

#[test]
fn dedupe_drops_repeated_content_and_numbers_the_rest() {
    let out = dedupe_session_turns(vec![
        result_turn(Some(2), "completed", " hello ", "hi", "r"),
        result_turn(Some(1), "completed", "hello", "hi ", "r"),
        result_turn(Some(3), "completed", "hello", "hi", "other"),
    ]);
    let numbers: Vec<Option<i64>> = out.iter().map(|t| t.turn).collect();
    assert_eq!(numbers, [Some(1), Some(3)]);

    let out = dedupe_session_turns(vec![
        result_turn(None, "completed", "x", "", "rx"),
        result_turn(Some(1), "completed", "y", "", "ry"),
        result_turn(None, "completed", "z", "", "rz"),
        result_turn(Some(0), "completed", "", "greeting", "r0"),
    ]);
    let order: Vec<(Option<i64>, &str)> = out
        .iter()
        .map(|t| (t.turn, t.request_id.as_str()))
        .collect();
    assert_eq!(
        order,
        [
            (Some(0), "r0"),
            (Some(1), "ry"),
            (Some(2), "rx"),
            (Some(3), "rz")
        ]
    );
    assert!(dedupe_session_turns(vec![]).is_empty());
}

// ---------------------------------------------------------------------------
// In-progress merge and is_active
// ---------------------------------------------------------------------------

#[test]
fn in_progress_result_creates_an_active_session() {
    let f = fixture();
    f.write_aged("old-session.json", json!({"status": "completed", "updated_at": ago(7200), "turns": [turn_json(1, "old-call", "a", "b")]}), 7200);
    f.write_aged(
        "req1-turn1.json",
        json!({"job": {"request_id": "req1", "call_id": "callX", "to_user": "1000", "turn": "1"}, "conversation_id": "convX",
               "status": "processing", "phase": "transcribed", "transcript": {"text": "hello", "language": "en", "duration": 1.5}}),
        3600,
    );
    // A finished result without a session file does not create a session.
    f.write("req2.json", json!({"job": {"request_id": "req2", "call_id": "callY"}, "status": "completed", "phase": "completed"}));

    let sessions = f.sessions();
    assert_eq!(sessions.len(), 2);
    let s = f.session("convX");
    assert!(s.is_active);
    assert_eq!(
        (
            s.status.as_str(),
            s.latest_phase.as_str(),
            s.latest_transcript.as_str()
        ),
        ("active", "transcribed", "hello")
    );
    assert_eq!(
        (
            s.call_id.as_deref(),
            s.target_number.as_deref(),
            s.turn_count
        ),
        (Some("callX"), Some("1000"), 1)
    );
    assert_eq!(
        (s.turns[0].turn, s.turns[0].duration_seconds),
        (Some(1), 1.5)
    );
    assert_eq!(s.started_at, s.updated_at);
    assert!(!f.session("old").is_active);
    assert_eq!(sessions[0].session_id, "convX");
}

#[test]
fn in_progress_result_updates_an_existing_session() {
    let f = fixture();
    f.write_aged(
        "conv1-session.json",
        json!({"conversation_id": "conv1", "status": "active", "updated_at": ago(600), "turns": [turn_json(1, "call1", "first", "ok")]}),
        600,
    );
    let pending = f.write_aged(
        "x2-turn2.json",
        json!({"job": {"request_id": "x2", "call_id": "call1"}, "conversation_id": "conv1", "turn": 2, "status": "processing", "phase": "reasoned",
               "reasoning": {"transcript": "second", "intent": "ask_fees", "response": "No fee", "slots": {"action": "continue"}}}),
        300,
    );
    let s = f.session("conv1");
    assert!(s.is_active);
    assert_eq!(
        (
            s.latest_transcript.as_str(),
            s.latest_intent.as_str(),
            s.latest_response.as_str()
        ),
        ("second", "ask_fees", "No fee")
    );
    assert_eq!(
        (
            s.latest_action.as_deref(),
            s.latest_phase.as_str(),
            s.turn_count
        ),
        (Some("continue"), "reasoned", 2)
    );
    assert_eq!(s.turns[1].request_id, "x2");
    assert_eq!(s.updated_at, s.turns[1].observed_at.clone().unwrap());

    // The same turn seen again in a later phase replaces the timeline entry.
    f.write_aged("conv1-session.json", json!({"conversation_id": "conv1", "status": "active", "updated_at": ago(600),
        "turns": [turn_json(1, "call1", "first", "ok"), {"turn": 2, "request_id": "x2", "transcript": "sec", "phase": "transcribed"}]}), 600);
    let s = f.session("conv1");
    assert_eq!(
        (
            s.turn_count,
            s.turns[1].phase.as_str(),
            s.turns[1].transcript.as_str()
        ),
        (2, "reasoned", "second")
    );

    // Once the turn completes, only the phase and latency are taken from the result.
    std::fs::remove_file(pending).unwrap();
    f.write_aged(
        "x1-turn1.json",
        json!({"job": {"request_id": "x1", "call_id": "call1"}, "conversation_id": "conv1", "turn": 1, "status": "completed", "phase": "completed",
               "reasoning": {"transcript": "ignored"}, "timings_ms": {"total": 950}}),
        300,
    );
    let s = f.session("conv1");
    assert_eq!(
        (
            s.latest_latency_ms,
            s.latest_phase.as_str(),
            s.latest_transcript.as_str()
        ),
        (Some(950), "completed", "sec")
    );
    assert!(!s.is_active);
}

#[test]
fn is_active_needs_recent_activity_and_a_non_terminal_status() {
    let f = fixture();
    let naive_now = Utc::now()
        .naive_utc()
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string();
    let one = |id: &str, status: &str, updated_at: String, phase: &str| {
        f.write_aged(
            &format!("{id}-session.json"),
            json!({"status": status, "updated_at": updated_at, "latest_phase": phase, "turns": [turn_json(1, id, "a", "b")]}),
            3600,
        );
    };
    one("fresh", "active", ago(5), "");
    one("edge", "active", ago(170), "");
    one("stale", "active", ago(600), "");
    one("naive", "active", naive_now, "");
    one("garbage", "active", "not a time".into(), "");
    one("busy", "active", ago(600), "processing");
    one("busy-failed", "failed", ago(600), "processing");
    for status in TERMINAL_STATUSES {
        one(&format!("done-{status}"), status, ago(5), "");
    }
    f.write(
        "mtime-session.json",
        json!({"turns": [turn_json(1, "m", "a", "b")]}),
    );

    let active: Vec<String> = f
        .sessions()
        .into_iter()
        .filter(|s| s.is_active)
        .map(|s| s.session_id)
        .collect();
    let mut sorted = active.clone();
    sorted.sort();
    assert_eq!(sorted, ["busy", "edge", "fresh", "mtime", "naive"]);
    assert_eq!(f.sessions().len(), 12);
    assert_eq!(f.session("stale").latest_phase, "completed");
}

#[test]
fn sessions_are_sorted_newest_first_by_parsed_time() {
    let f = fixture();
    let one = |id: &str, updated_at: &str| {
        f.write(&format!("{id}-session.json"), json!({"status": "completed", "updated_at": updated_at, "turns": [turn_json(1, id, "a", "b")]}));
    };
    one("zulu", "2026-07-08T01:00:00Z");
    one("offset", "2026-07-08T05:00:00+10:00");
    one("naive", "2026-07-08 02:00:00");
    one("date", "2026-07-09");
    one("junk", "zzz");
    let order: Vec<String> = f.sessions().into_iter().map(|s| s.session_id).collect();
    assert_eq!(order, ["date", "naive", "zulu", "offset", "junk"]);
    let limited: Vec<String> = f
        .service
        .list_live_call_sessions(2)
        .into_iter()
        .map(|s| s.session_id)
        .collect();
    assert_eq!(limited.len(), 2);

    assert_eq!(
        parse_iso("2026-07-08T01:00:00.123456+00:00"),
        parse_iso("2026-07-08T01:00:00.123456Z")
    );
    assert_eq!(
        parse_iso("2026-07-08T01:00:00"),
        parse_iso("2026-07-08T01:00:00Z")
    );
    assert_eq!(
        parse_iso("2026-07-08T03:00:00+0200"),
        parse_iso("2026-07-08T01:00:00Z")
    );
    assert_eq!(parse_iso(""), None);
}

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

#[test]
fn session_is_found_by_session_id_file_name_and_call_ids() {
    let f = fixture();
    f.write(
        "file-a-session.json",
        json!({"conversation_id": "conv-a", "status": "completed", "turns": [turn_json(1, "c-old", "a", "b"), turn_json(2, "c-new", "c", "d")]}),
    );
    f.write("gone-session.json", json!({"conversation_id": "gone", "status": "completed", "started_at": "2026-07-08T01:00:00Z",
        "slots": {"action": "callback"}, "turns": []}));

    for id in ["conv-a", "c-new", "c-old", "file-a"] {
        let s = f
            .service
            .get_session_by_id(id)
            .unwrap_or_else(|| panic!("lookup {id}"));
        assert_eq!(
            (s.session_id.as_str(), s.call_id.as_deref()),
            ("conv-a", Some("c-new")),
            "{id}"
        );
    }
    // An ended call with no turns is hidden from the list but still reachable by id.
    assert!(f.sessions().iter().all(|s| s.session_id != "gone"));
    let s = f.service.get_session_by_id("gone").unwrap();
    assert_eq!(
        (
            s.session_id.as_str(),
            s.call_id.as_deref(),
            s.status.as_str()
        ),
        ("gone", Some("gone"), "completed")
    );
    assert_eq!(
        (s.turn_count, s.is_active, s.latest_action.as_deref()),
        (0, false, Some("callback"))
    );
    assert_eq!(s.started_at, "2026-07-08T01:00:00Z");

    for id in ["", "missing", "../results/file-a", "..\\file-a"] {
        assert!(f.service.get_session_by_id(id).is_none(), "{id:?}");
    }
}

// ---------------------------------------------------------------------------
// Recordings
// ---------------------------------------------------------------------------

#[test]
fn recording_is_resolved_from_path_then_channel_id() {
    let f = fixture();
    let with = |recording_path: &str, channel_uuid: &str| {
        let mut s = LiveCallSession::new("s");
        s.recording_path = recording_path.into();
        s.channel_uuid = channel_uuid.into();
        s
    };
    let good = f.recording("rec1-call.wav", 100);
    f.recording("header-call.wav", 44);
    let by_channel = f.recording("chan9-call.wav", 45);
    let loose = f.recording("2026-chan7-part-call.wav", 100);
    f.recording("chan5-call.wav", 10);

    assert_eq!(
        f.service
            .resolve_recording_file(&with("/var/lib/voice-ai/pilot/media/rec1-call.wav", "")),
        Some(good.clone())
    );
    assert_eq!(
        f.service
            .resolve_recording_file(&with(r"C:\pilot\media\rec1-call.wav", "")),
        Some(good.clone())
    );
    assert_eq!(
        f.service
            .resolve_recording_file(&with("/var/lib/voice-ai/pilot/media/header-call.wav", "")),
        None
    );
    assert_eq!(
        f.service
            .resolve_recording_file(&with("/elsewhere/missing.wav", "chan9")),
        Some(by_channel)
    );
    assert_eq!(
        f.service.resolve_recording_file(&with("", "chan7")),
        Some(loose)
    );
    assert_eq!(f.service.resolve_recording_file(&with("", "chan5")), None);
    assert_eq!(f.service.resolve_recording_file(&with("", "")), None);
    assert_eq!(
        f.service
            .resolve_recording_file(&with("/elsewhere/missing.wav", "nope")),
        None
    );
    // A path outside the media directory is used as given when it exists there.
    let outside = f.dir.path().join("outside.wav");
    std::fs::write(&outside, vec![0u8; 64]).unwrap();
    assert_eq!(
        f.service
            .resolve_recording_file(&with(&outside.to_string_lossy(), "")),
        Some(outside)
    );

    f.write(
        "r1-session.json",
        json!({"recording_path": "/var/lib/voice-ai/pilot/media/rec1-call.wav", "turns": []}),
    );
    f.write(
        "r2-session.json",
        json!({"channel_uuid": "chan9", "turns": []}),
    );
    f.write(
        "r3-session.json",
        json!({"recording_path": "/var/lib/voice-ai/pilot/media/header-call.wav", "turns": []}),
    );
    assert!(f.session("r1").has_recording);
    assert!(f.session("r2").has_recording);
    assert!(!f.session("r3").has_recording);
    assert_eq!(
        f.service.resolve_recording_file(&f.session("r1")),
        Some(good)
    );
}

// ---------------------------------------------------------------------------
// Post analysis
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ensure_post_analysis_merges_persists_and_builds_content() {
    let server = MockServer::start().await;
    chat_mock(
        &server,
        json!({"primary_intent": "ask_fees", "summary": "Caller asked about fees.", "outcome": "completed",
               "next_best_action": "callback", "recovered_slots": {"amount": "$900"}}),
    )
    .await;
    let f = fixture_with(&server.uri(), "test-key");
    let path = f.write(
        "conv-a-session.json",
        json!({"conversation_id": "conv-a", "status": "completed", "updated_at": "2026-07-08T01:05:00.000Z", "extra": {"kept": true},
               "slots": {"debt_type": "invoice"}, "turns": [turn_json(1, "call-a", "What are your fees?", "No joining fee.")]}),
    );

    let s = f
        .service
        .ensure_post_analysis("conv-a", false)
        .await
        .unwrap();
    let analysis = s.post_analysis.clone().unwrap();
    assert!(s.has_post_analysis);
    assert_eq!(
        (
            analysis.primary_intent.as_str(),
            analysis.summary.as_str(),
            analysis.error.as_str()
        ),
        ("ask_fees", "Caller asked about fees.", "")
    );
    assert_eq!(
        Value::Object(analysis.recovered_slots.clone()),
        json!({"debt_type": "invoice", "amount": "$900"})
    );
    assert!(analysis
        .internal_handoff_brief
        .starts_with("Caller goal: What are your fees?"));
    assert_eq!(s.handoff_brief, analysis.internal_handoff_brief);
    let content = s.content.clone().unwrap();
    assert_eq!(content.artifacts.len(), 5);
    assert!(content.artifacts[2].body.contains("about invoice ($900)"));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["response_format"]["type"], "json_object");
    assert_eq!(body["messages"][0]["content"], POST_ANALYSIS_PROMPT);
    let conversation = body["messages"][1]["content"].as_str().unwrap();
    assert!(conversation.starts_with("Session: conv-a\n"));
    assert!(conversation
        .contains("[Caller T1] What are your fees?\n[AI T1] (intent=new_enquiry) No joining fee."));

    // Written back to the session file, keeping everything else in it.
    let stored = read_json(&path).unwrap();
    assert_eq!(
        stored["post_analysis"],
        serde_json::to_value(&analysis).unwrap()
    );
    assert_eq!(stored["content"], serde_json::to_value(&content).unwrap());
    assert_eq!(
        stored["handoff_brief"],
        json!(analysis.internal_handoff_brief)
    );
    assert_eq!(stored["extra"], json!({"kept": true}));
    assert_eq!(stored["turns"].as_array().unwrap().len(), 1);
    assert_ne!(stored["updated_at"], "2026-07-08T01:05:00.000Z");
    assert!(parse_iso(stored["updated_at"].as_str().unwrap()).is_some());
    let leftovers: Vec<_> = std::fs::read_dir(f.results())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(leftovers.len(), 1, "{leftovers:?}");

    // A stored analysis is reused; force asks the model again.
    let again = f
        .service
        .ensure_post_analysis("call-a", false)
        .await
        .unwrap();
    assert_eq!(
        again.post_analysis.unwrap().generated_at,
        analysis.generated_at
    );
    assert_eq!(again.content.unwrap().generated_at, content.generated_at);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let forced = f
        .service
        .ensure_post_analysis("conv-a", true)
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    assert_eq!(forced.content.unwrap().artifacts.len(), 5);
}

#[tokio::test]
async fn ensure_post_analysis_without_a_key_uses_the_heuristic() {
    let f = fixture();
    let path = f.write(
        "conv-b-session.json",
        json!({"conversation_id": "conv-b", "status": "completed", "slots": {"action": "decline_debt_type", "debt_type": "rental"},
               "turns": [turn_json(1, "conv-b", "My tenant owes rent", "We cannot take that.")]}),
    );
    let s = f
        .service
        .ensure_post_analysis("conv-b", false)
        .await
        .unwrap();
    let analysis = s.post_analysis.clone().unwrap();
    assert!(analysis.error.contains("XAI_API_KEY"), "{}", analysis.error);
    assert_eq!(
        (analysis.status.as_str(), analysis.outcome.as_str()),
        ("ready", "declined_unsupported")
    );
    assert_eq!(analysis.risks, ["unsupported_debt", "no_amount"]);
    assert!(s.has_post_analysis);
    let keys: Vec<&str> = s
        .content
        .as_ref()
        .unwrap()
        .artifacts
        .iter()
        .map(|a| a.key.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "follow_up_email",
            "handoff_brief",
            "follow_up_sms",
            "fee_faq",
            "unsupported_lod_checklist",
            "enquiry_log_json"
        ]
    );

    let stored = read_json(&path).unwrap();
    assert_eq!(stored["post_analysis"]["error"], json!(analysis.error));
    assert_eq!(stored["content"]["artifacts"].as_array().unwrap().len(), 6);
    let reloaded = f.service.get_session_by_id("conv-b").unwrap();
    assert_eq!(reloaded.post_analysis, Some(analysis));
    assert_eq!(reloaded.content, s.content);

    let err = f
        .service
        .ensure_post_analysis("nope", false)
        .await
        .unwrap_err();
    assert!(matches!(err, ServiceError::NotFound(_)));
    assert_eq!(err.to_string(), "Session not found: nope");
}

#[tokio::test]
async fn model_failures_fall_back_to_the_heuristic() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_json(json!({"error": {"message": "out of credit"}})),
        )
        .mount(&server)
        .await;
    let f = fixture_with(&server.uri(), "k");
    let mut s = LiveCallSession::new("s1");
    s.mode = "roleplay".into();
    let analysis = f.service.generate_post_analysis(&s).await;
    assert_eq!(analysis.error, "xAI 429: out of credit");
    assert_eq!(analysis.primary_intent, "roleplay_invoice");
    assert!(analysis.roleplay_scorecard.is_some());
    let body: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        crate::prompts::ROLEPLAY_ANALYSIS_PROMPT
    );

    let server = MockServer::start().await;
    chat_mock(&server, json!(["not", "an", "object"])).await;
    let f = fixture_with(&server.uri(), "k");
    let analysis = f
        .service
        .generate_post_analysis(&LiveCallSession::new("s2"))
        .await;
    assert!(analysis.error.contains("not an object"));
    assert_eq!(analysis.status, "ready");
}

// ---------------------------------------------------------------------------
// Fingerprint and directory signature
// ---------------------------------------------------------------------------

#[test]
fn fingerprint_changes_with_any_visible_field() {
    let mut s = LiveCallSession::new("s1");
    s.updated_at = "u".into();
    s.latest_phase = "p".into();
    s.latest_transcript = "t".into();
    s.latest_response = "r".into();
    s.turn_count = 1;
    s.turns = vec![result_turn(Some(1), "completed", "t", "r", "r1")];
    let base = sessions_fingerprint(std::slice::from_ref(&s));
    assert_eq!(base, "s1|active|u|p|t|r||1|completed|r1");
    assert_eq!(
        sessions_fingerprint(&[s.clone(), LiveCallSession::new("s2")]),
        format!("{base}\ns2|active||||||0||")
    );
    assert_eq!(sessions_fingerprint(&[]), "");

    let changes: [fn(&mut LiveCallSession); 8] = [
        |s| s.status = "completed".into(),
        |s| s.updated_at = "later".into(),
        |s| s.latest_phase = "reasoned".into(),
        |s| s.latest_transcript.push('!'),
        |s| s.latest_response.push('!'),
        |s| s.latest_action = Some("transfer".into()),
        |s| s.turns[0].phase = "reasoned".into(),
        |s| s.turns[0].request_id = "r2".into(),
    ];
    for change in changes {
        let mut changed = s.clone();
        change(&mut changed);
        assert_ne!(sessions_fingerprint(&[changed]), base);
    }
    let mut unchanged = s.clone();
    unchanged.latest_intent = "not shown".into();
    assert_eq!(sessions_fingerprint(&[unchanged]), base);
}

#[test]
fn results_dir_signature_tracks_count_and_newest_change() {
    let f = fixture();
    assert_eq!(f.service.results_dir_signature(), "empty");
    std::fs::write(f.results().join("notes.txt"), "x").unwrap();
    assert_eq!(f.service.results_dir_signature(), "empty");
    let path = f.write_aged("a-session.json", json!({}), 100);
    let first = f.service.results_dir_signature();
    let (count, newest) = first.split_once(':').unwrap();
    assert_eq!(count, "1");
    assert!(newest.parse::<u128>().unwrap() > 1_000_000_000_000_000_000);
    assert_eq!(f.service.results_dir_signature(), first);
    set_age(&path, 50);
    assert_ne!(f.service.results_dir_signature(), first);
    f.write_aged("b.json", json!({}), 500);
    assert!(f.service.results_dir_signature().starts_with("2:"));

    std::fs::remove_dir_all(f.results()).unwrap();
    assert_eq!(f.service.results_dir_signature(), "missing");
    assert!(f.sessions().is_empty());
    assert!(f.service.get_session_by_id("a").is_none());
}

#[test]
fn client_pack_and_directories() {
    let f = fixture();
    f.service.ensure_dirs();
    let settings = f.service.settings();
    for dir in [
        settings.artifacts_dir.join("uploads"),
        settings.artifacts_dir.join("results"),
        settings.brand_dir.clone(),
    ] {
        assert!(dir.is_dir(), "{dir:?}");
    }
    let clients = settings.clients_dir.clone();
    std::fs::create_dir_all(clients.join("default")).unwrap();
    std::fs::create_dir_all(clients.join("listy")).unwrap();
    std::fs::write(
        clients.join("default/client.json"),
        r#"{"id": "default", "company": "Example"}"#,
    )
    .unwrap();
    std::fs::write(clients.join("listy/client.json"), "[1]").unwrap();
    assert_eq!(f.service.load_client_pack(None)["company"], "Example");
    assert_eq!(f.service.load_client_pack(Some(""))["id"], "default");
    assert_eq!(
        f.service.load_client_pack(Some(" default "))["id"],
        "default"
    );
    assert_eq!(f.service.load_client_pack(Some("listy")), json!({}));
    assert_eq!(f.service.load_client_pack(Some("missing")), json!({}));
}

// ---------------------------------------------------------------------------
// Device upload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn device_upload_is_saved_transcribed_and_reasoned() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(url_path("/stt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "text": "hello there", "language": "en-AU", "duration": 1.5,
            "words": [{"text": "hello", "start": 0.0, "end": 0.4}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(url_path("/chat/completions"))
        .and(body_partial_json(json!({"response_format": {"type": "json_schema", "json_schema": {"name": "ReasoningResult", "strict": true}}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices": [{"message": {"content":
            json!({"transcript": "Hello there.", "intent": "greeting", "response": "Hi!"}).to_string()}}]})))
        .mount(&server)
        .await;
    let f = fixture_with(&server.uri(), "test-key");

    let done = f
        .service
        .process_device_upload(r"C:\recordings\clip.MP3", b"RIFFfake".to_vec())
        .await
        .unwrap();
    let upload = PathBuf::from(&done.saved_upload_path);
    assert_eq!(
        upload.parent().unwrap(),
        f.service.settings().artifacts_dir.join("uploads")
    );
    assert_eq!(upload.extension().unwrap(), "MP3");
    assert_eq!(std::fs::read(&upload).unwrap(), b"RIFFfake");

    let i = &done.interaction;
    assert_eq!(
        (
            i.source.as_str(),
            i.transcript.as_str(),
            i.intent.as_str(),
            i.response.as_str()
        ),
        ("device", "Hello there.", "greeting", "Hi!")
    );
    assert_eq!(
        (i.language.as_str(), i.duration_seconds, i.provider.as_str()),
        ("en-AU", 1.5, "xai-stt")
    );
    assert_eq!(
        (i.status.as_str(), i.phase.as_str()),
        ("completed", "reasoned")
    );
    assert_eq!(
        i.input_audio_path.as_deref(),
        Some(done.saved_upload_path.as_str())
    );
    assert_eq!(i.observed_at.as_deref(), Some(i.timestamp.as_str()));

    let result_path = PathBuf::from(&done.saved_result_path);
    assert_eq!(
        result_path,
        f.service
            .settings()
            .artifacts_dir
            .join("results")
            .join(format!("{}.json", i.request_id))
    );
    let record = read_json(&result_path).unwrap();
    assert_eq!(
        record.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["interaction", "transcript_event", "reasoning_result"]
    );
    assert_eq!(record["interaction"], serde_json::to_value(i).unwrap());
    assert_eq!(
        record["transcript_event"],
        json!({"request_id": i.request_id, "timestamp": i.timestamp, "transcript": "hello there", "language": "en-AU",
               "duration_seconds": 1.5, "provider": "xai-stt", "words": [{"text": "hello", "start": 0.0, "end": 0.4, "speaker": null}]})
    );
    assert_eq!(
        record["reasoning_result"],
        json!({"transcript": "Hello there.", "intent": "greeting", "response": "Hi!"})
    );

    let chat = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.url.path() == "/chat/completions")
        .unwrap();
    let body: Value = serde_json::from_slice(&chat.body).unwrap();
    assert_eq!(body["messages"][0]["content"], DEVICE_SYSTEM_PROMPT);
    assert_eq!(
        body["messages"][1],
        json!({"role": "user", "content": "hello there"})
    );
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["required"],
        json!(["transcript", "intent", "response"])
    );

    assert_eq!(upload_suffix(""), ".wav");
    assert_eq!(upload_suffix("noext"), ".wav");
    assert_eq!(upload_suffix(".hidden"), ".wav");
    assert_eq!(upload_suffix("dir.d/clip.m4a"), ".m4a");
}

#[tokio::test]
async fn device_upload_reports_failures() {
    let f = fixture();
    let err = f
        .service
        .process_device_upload("a.wav", vec![1, 2, 3])
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ServiceError::Other(message) if message.contains("XAI_API_KEY")),
        "{err}"
    );

    let server = MockServer::start().await;
    Mock::given(url_path("/stt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "hi"})))
        .mount(&server)
        .await;
    chat_mock(&server, json!({"transcript": "hi", "intent": "greeting"})).await;
    let f = fixture_with(&server.uri(), "k");
    let err = f
        .service
        .process_device_upload("a.wav", vec![1, 2, 3])
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "Reasoning result is missing \"response\"");
}

// ---------------------------------------------------------------------------
// Real data (read-only)
// ---------------------------------------------------------------------------

#[test]
fn real_pilot_results_load_without_panicking() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let results = root.join("docker/sip-stack/runtime/pilot/results");
    if !results.is_dir() {
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let mut settings = ConsoleSettings::for_dir(scratch.path());
    settings.telephony_results_dir = results;
    settings.telephony_media_dir = root.join("docker/sip-stack/runtime/pilot/media");
    let service = SessionService::new(settings, XaiRest::new(XaiSettings::from_env()));

    let sessions = service.list_live_call_sessions(200);
    assert!(!sessions.is_empty());
    for session in &sessions {
        assert!(!session.session_id.is_empty());
        assert_eq!(session.turn_count as usize, session.turns.len());
        let _ = conversation_for_analysis(session);
        let _ = heuristic_post_analysis(session);
        let _ = generate_call_content(session, None);
        let _ = service.resolve_recording_file(session);
    }
    let times: Vec<_> = sessions
        .iter()
        .filter_map(|s| parse_iso(&s.updated_at))
        .collect();
    assert!(times.windows(2).all(|pair| pair[0] >= pair[1]));
    assert!(!sessions_fingerprint(&sessions).is_empty());
    let _ = service.list_recent_telephony_results(200);
    let first = service.get_session_by_id(&sessions[0].session_id);
    assert_eq!(
        first.map(|s| s.session_id),
        Some(sessions[0].session_id.clone())
    );
    assert!(service.results_dir_signature().contains(':'));
    if let Ok(dump) = std::env::var("VOICE_SESSIONS_DUMP") {
        // Opt-in dump for comparing against the Python console by hand.
        let derived: Vec<Value> = sessions
            .iter()
            .map(|s| {
                json!({"conversation": conversation_for_analysis(s), "heuristic": heuristic_post_analysis(s),
                       "content": generate_call_content(s, None)})
            })
            .collect();
        let fingerprint = sessions_fingerprint(&sessions);
        let all = json!({"sessions": sessions, "derived": derived, "fingerprint": fingerprint});
        std::fs::write(dump, all.to_string()).unwrap();
    }
}
