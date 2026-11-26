//! Ingest existing call recordings into console sessions: transcribe each file,
//! split it into turns by speaker, save it as a session and run the post-call summary.
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use voice_core::xai::{rest::TranscriptWord, XaiRest};
use voice_sessions::{service::SessionService, settings::ConsoleSettings};

const AUDIO_SUFFIXES: [&str; 5] = ["wav", "mp3", "m4a", "ogg", "flac"];

/// What a phone-system recording file name tells us about the call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileMeta {
    pub filename: String,
    pub ext: String,
    pub number: String,
    pub direction: String,
    pub started_at: String,
    pub uid: String,
}

/// Parse the three recording name patterns used by the phone system.
pub fn parse_filename(path: &Path) -> FileMeta {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let mut meta = FileMeta {
        filename: path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string(),
        direction: "unknown".into(),
        ..Default::default()
    };
    let patterns = [
        r"^(?P<date>\d{8})-(?P<time>\d{6})-(?P<number>\d+)-(?P<direction>outgoing|incoming|inbound)-(?P<ext>\d+)$",
        r"^(?P<direction>external|internal)-(?P<ext>\d+)-(?P<number>\d+)-(?P<date>\d{8})-(?P<time>\d{6})-(?P<uid>[\d.]+)$",
        r"^out-(?P<number>\d+)-(?P<ext>\d+)-(?P<date>\d{8})-(?P<time>\d{6})-(?P<uid>[\d.]+)$",
    ];
    let Some(found) = patterns
        .iter()
        .find_map(|p| Regex::new(p).expect("static pattern").captures(stem))
    else {
        return meta;
    };
    let group = |name: &str| {
        found
            .name(name)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default()
    };
    meta.ext = group("ext");
    meta.number = group("number");
    meta.uid = group("uid");
    let direction = group("direction").to_lowercase();
    meta.direction = match direction.as_str() {
        "out" | "outgoing" => "outbound".into(),
        _ if stem.starts_with("out-") => "outbound".into(),
        "external" | "incoming" | "inbound" => "inbound".into(),
        "internal" => "internal".into(),
        "" => "unknown".into(),
        other => other.to_string(),
    };
    let stamp = format!("{}{}", group("date"), group("time"));
    if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(&stamp, "%Y%m%d%H%M%S") {
        meta.started_at = parsed.format("%Y-%m-%dT%H:%M:%S+00:00").to_string();
    }
    meta
}

pub fn session_id_for(meta: &FileMeta, digest: &str) -> String {
    if !meta.uid.is_empty() {
        return format!("sbr-{}", meta.uid.replace('.', "-"));
    }
    let stamp: String = meta
        .started_at
        .chars()
        .take(19)
        .collect::<String>()
        .replace([':', '-'], "")
        .replace('T', "-");
    let stamp = if stamp.is_empty() {
        digest.chars().take(10).collect()
    } else {
        stamp
    };
    let ext = if meta.ext.is_empty() { "x" } else { &meta.ext };
    format!("sbr-{stamp}-{ext}")
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn turn(number: usize, transcript: &str, action: &str) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "turn": number,
        "transcript": transcript.trim(),
        "intent": "offline_recording",
        "response": "",
        "action": action,
    }) else {
        unreachable!()
    };
    map
}

/// Split a transcript into turns wherever the speaker changes. With fewer than two
/// speakers the whole transcript is one turn.
pub fn group_turns(transcript: &str, words: &[TranscriptWord]) -> Vec<Map<String, Value>> {
    // Speaker 0 and "no speaker" are both treated as unlabelled, as in the original.
    let label = |w: &TranscriptWord| {
        w.speaker
            .filter(|s| *s != 0)
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    let mut unique: Vec<String> = vec![];
    for word in words.iter().filter(|w| !w.text.is_empty()) {
        let speaker = label(word);
        if !speaker.is_empty() && !unique.contains(&speaker) {
            unique.push(speaker);
        }
    }
    if unique.len() < 2 {
        return vec![turn(1, transcript, "complete")];
    }
    let mut turns = vec![];
    let mut current = unique[0].clone();
    let mut parts: Vec<&str> = vec![];
    for word in words {
        let text = word.text.trim();
        if text.is_empty() {
            continue;
        }
        let speaker = Some(label(word))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| current.clone());
        if speaker != current && !parts.is_empty() {
            turns.push(turn(turns.len() + 1, &parts.join(" "), "continue"));
            parts.clear();
            current = speaker;
        }
        parts.push(text);
    }
    if !parts.is_empty() {
        turns.push(turn(turns.len() + 1, &parts.join(" "), "complete"));
    }
    match turns.last_mut() {
        Some(last) => {
            last.insert("action".into(), json!("complete"));
            turns
        }
        None => vec![turn(1, transcript, "complete")],
    }
}

fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

/// Build the session document for one ingested recording.
pub fn session_document(
    session_id: &str,
    meta: &FileMeta,
    transcript: &str,
    words: &[TranscriptWord],
    media_name: &str,
) -> Value {
    let now = now_iso();
    let mut turns = group_turns(transcript, words);
    for t in &mut turns {
        let number = t["turn"].as_u64().unwrap_or(1);
        t.insert("call_id".into(), json!(session_id));
        t.insert("target_number".into(), json!(meta.number));
        t.insert("prompt_profile".into(), json!("sop_enquiries"));
        t.insert(
            "request_id".into(),
            json!(format!("{session_id}-t{number}")),
        );
    }
    let latest_transcript = turns
        .last()
        .and_then(|t| t["transcript"].as_str())
        .filter(|t| !t.is_empty())
        .unwrap_or(transcript)
        .to_string();
    json!({
        "conversation_id": session_id,
        "target_number": meta.number,
        "prompt_profile": "sop_enquiries",
        "status": "completed",
        "started_at": if meta.started_at.is_empty() { now.clone() } else { meta.started_at.clone() },
        "updated_at": now,
        "mode": "offline",
        "source": "sbr_audio_ingest",
        "staff_extension": meta.ext,
        "direction": meta.direction,
        "original_filename": meta.filename,
        "slots": {},
        "latest_intent": "offline_recording",
        "latest_transcript": latest_transcript,
        "latest_response": "",
        "latest_action": "complete",
        "recording_path": format!("/var/lib/voice-ai/pilot/media/{media_name}"),
        "channel_uuid": session_id,
        "turns": turns,
    })
}

pub struct IngestOptions {
    pub source_dir: PathBuf,
    pub out_dir: PathBuf,
    pub clients_dir: PathBuf,
    pub client_id: String,
}

/// Ingest every audio file in `source_dir`. Returns how many were fully summarised.
pub async fn run(xai: &XaiRest, options: &IngestOptions) -> anyhow::Result<usize> {
    if !options.source_dir.is_dir() {
        anyhow::bail!("Audio folder not found: {}", options.source_dir.display());
    }
    let results_dir = options.out_dir.join("results");
    let media_dir = options.out_dir.join("media");
    std::fs::create_dir_all(&results_dir)?;
    std::fs::create_dir_all(&media_dir)?;

    let mut files: Vec<PathBuf> = std::fs::read_dir(&options.source_dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| AUDIO_SUFFIXES.contains(&e.to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    println!("Found {} audio files", files.len());

    let service = SessionService::new(
        ConsoleSettings {
            artifacts_dir: options.out_dir.clone(),
            telephony_results_dir: results_dir.clone(),
            telephony_media_dir: media_dir.clone(),
            active_window_seconds: 180,
            brand_dir: options.out_dir.join("brand"),
            clients_dir: options.clients_dir.clone(),
            default_client_id: options.client_id.clone(),
            base_path: String::new(),
        },
        xai.clone(),
    );

    let mut seen: HashMap<String, String> = HashMap::new();
    let mut manifest: Vec<Value> = vec![];
    for path in &files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let digest = sha256_file(path)?;
        if let Some(original) = seen.get(&digest) {
            println!("SKIP duplicate of {original}: {name}");
            manifest.push(json!({"filename": name, "status": "duplicate", "of": original}));
            continue;
        }
        let meta = parse_filename(path);
        let sid = session_id_for(&meta, &digest);
        let suffix = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("wav")
            .to_lowercase();
        let media_name = format!("{sid}-call.{suffix}");
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        println!(
            "STT {name} -> {sid} ({size} bytes, {} {})",
            meta.direction, meta.number
        );

        let event = match xai.transcribe_file(path).await {
            Ok(event) => event,
            Err(error) => {
                println!("FAIL STT {name}: {error}");
                manifest.push(
                    json!({"filename": name, "status": "stt_failed", "error": error.to_string()}),
                );
                continue;
            }
        };
        let transcript = event.text.trim().to_string();
        if transcript.is_empty() {
            println!("FAIL empty transcript: {name}");
            manifest
                .push(json!({"filename": name, "status": "empty_transcript", "session_id": sid}));
            continue;
        }
        std::fs::copy(path, media_dir.join(&media_name))?;
        let document = session_document(&sid, &meta, &transcript, &event.words, &media_name);
        std::fs::write(
            results_dir.join(format!("{sid}-session.json")),
            serde_json::to_string_pretty(&document)?,
        )?;

        match service.ensure_post_analysis(&sid, true).await {
            Ok(session) => {
                let analysis = session.post_analysis.unwrap_or_default();
                let short: String = analysis.summary.chars().take(160).collect();
                println!("OK  {sid}  intent={}  {short}", analysis.primary_intent);
                manifest.push(json!({
                    "filename": name, "status": "ready", "session_id": sid,
                    "direction": meta.direction, "number": meta.number, "ext": meta.ext,
                    "duration_seconds": event.duration,
                    "primary_intent": analysis.primary_intent, "summary": analysis.summary,
                    "recording": media_name,
                }));
            }
            Err(error) => {
                println!("WARN session saved, summary failed {sid}: {error}");
                manifest.push(json!({
                    "filename": name, "status": "transcribed", "session_id": sid,
                    "error": error.to_string(), "recording": media_name,
                }));
            }
        }
        seen.insert(digest, name);
    }

    let manifest_path = options.out_dir.join("manifest.json");
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;
    let ready = manifest
        .iter()
        .filter(|item| item["status"] == "ready")
        .count();
    println!(
        "Done. {ready}/{} summarised. Manifest: {}",
        files.len(),
        manifest_path.display()
    );
    Ok(ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use voice_core::config::XaiSettings;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn word(text: &str, speaker: Option<i64>) -> TranscriptWord {
        TranscriptWord {
            text: text.into(),
            start: 0.0,
            end: 0.1,
            speaker,
        }
    }

    #[test]
    fn file_name_patterns() {
        let clock = parse_filename(Path::new("20260910-093015-0412345678-outgoing-201.wav"));
        assert_eq!(
            (
                clock.direction.as_str(),
                clock.number.as_str(),
                clock.ext.as_str()
            ),
            ("outbound", "0412345678", "201")
        );
        assert_eq!(clock.started_at, "2026-09-10T09:30:15+00:00");
        assert_eq!(
            session_id_for(&clock, "abcdef0123456789"),
            "sbr-20260910-093015-201"
        );

        let external = parse_filename(Path::new(
            "external-202-0733334444-20260910-101500-1757499300.123.mp3",
        ));
        assert_eq!(
            (external.direction.as_str(), external.uid.as_str()),
            ("inbound", "1757499300.123")
        );
        assert_eq!(session_id_for(&external, "x"), "sbr-1757499300-123");

        let out = parse_filename(Path::new(
            "out-0733334444-203-20260910-110000-1757502000.9.wav",
        ));
        assert_eq!(
            (out.direction.as_str(), out.ext.as_str()),
            ("outbound", "203")
        );

        let unknown = parse_filename(Path::new("meeting notes.wav"));
        assert_eq!(
            (unknown.direction.as_str(), unknown.started_at.as_str()),
            ("unknown", "")
        );
        assert_eq!(
            session_id_for(&unknown, "abcdef0123456789"),
            "sbr-abcdef0123-x"
        );
    }

    #[test]
    fn turns_split_on_speaker_change() {
        let words = [
            word("Hello", Some(1)),
            word("there", Some(1)),
            word("Hi", Some(2)),
            word("Thanks", Some(1)),
        ];
        let turns = group_turns("Hello there Hi Thanks", &words);
        let texts: Vec<_> = turns
            .iter()
            .map(|t| t["transcript"].as_str().unwrap())
            .collect();
        assert_eq!(texts, ["Hello there", "Hi", "Thanks"]);
        let actions: Vec<_> = turns
            .iter()
            .map(|t| t["action"].as_str().unwrap())
            .collect();
        assert_eq!(actions, ["continue", "continue", "complete"]);

        let single = group_turns(
            " one speaker only ",
            &[word("one", Some(1)), word("speaker", None)],
        );
        assert_eq!(single.len(), 1);
        assert_eq!(single[0]["transcript"], "one speaker only");
        assert_eq!(single[0]["action"], "complete");
    }

    #[tokio::test]
    async fn ingests_a_folder_and_skips_duplicates() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/stt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "text": "I have an unpaid invoice. Sure, what amount?", "language": "en", "duration": 4.0,
                "words": [
                    {"text": "I", "start": 0.0, "end": 0.1, "speaker": 1}, {"text": "have", "start": 0.1, "end": 0.2, "speaker": 1},
                    {"text": "an", "start": 0.2, "end": 0.3, "speaker": 1}, {"text": "unpaid", "start": 0.3, "end": 0.4, "speaker": 1},
                    {"text": "invoice.", "start": 0.4, "end": 0.5, "speaker": 1},
                    {"text": "Sure,", "start": 1.0, "end": 1.1, "speaker": 2}, {"text": "what", "start": 1.1, "end": 1.2, "speaker": 2},
                    {"text": "amount?", "start": 1.2, "end": 1.3, "speaker": 2}
                ]
            })))
            .mount(&server)
            .await;
        let analysis = json!({"primary_intent": "new_enquiry", "summary": "Caller has an unpaid invoice.", "outcome": "completed"});
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"choices": [{"message": {"content": analysis.to_string()}}]}),
            ))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("audio");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("20260910-093015-0412345678-incoming-201.wav"),
            b"RIFF-one",
        )
        .unwrap();
        std::fs::write(source.join("copy-of-the-same-call.wav"), b"RIFF-one").unwrap();
        std::fs::write(source.join("notes.txt"), b"ignored").unwrap();

        let xai = XaiRest::new(XaiSettings {
            api_key: "k".into(),
            base_url: server.uri(),
            realtime_url: String::new(),
            grok_model: "m".into(),
            tts_voice: "eve".into(),
            language: "en".into(),
            stt_keyterm: None,
        });
        let options = IngestOptions {
            source_dir: source,
            out_dir: dir.path().join("out"),
            clients_dir: dir.path().join("clients"),
            client_id: "sbr".into(),
        };
        assert_eq!(run(&xai, &options).await.unwrap(), 1);

        let sid = "sbr-20260910-093015-201";
        let session: Value = serde_json::from_str(
            &std::fs::read_to_string(
                options
                    .out_dir
                    .join("results")
                    .join(format!("{sid}-session.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(session["mode"], "offline");
        assert_eq!(session["direction"], "inbound");
        assert_eq!(session["turns"].as_array().unwrap().len(), 2);
        assert_eq!(session["turns"][1]["request_id"], format!("{sid}-t2"));
        assert_eq!(
            session["post_analysis"]["summary"],
            "Caller has an unpaid invoice."
        );
        assert!(!session["content"]["artifacts"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(options
            .out_dir
            .join("media")
            .join(format!("{sid}-call.wav"))
            .exists());

        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(options.out_dir.join("manifest.json")).unwrap(),
        )
        .unwrap();
        let statuses: Vec<_> = manifest
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["ready", "duplicate"]);
    }
}
