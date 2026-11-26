//! The console's view of the voice agent's files: live call sessions, recordings,
//! post-call analysis with write-back, and device uploads.
use crate::analysis::{
    analysis_system_prompt, conversation_for_analysis, generate_call_content,
    heuristic_post_analysis, latency_ms, post_analysis_from_model,
};
use crate::models::{
    utc_now_iso, CallContentPack, CallPostAnalysis, DeviceSubmissionResult, LiveCallSession,
    VoiceInResult,
};
use crate::prompts::DEVICE_SYSTEM_PROMPT;
use crate::pyjson::{
    basename, dict, get, opt_str, or_chain, py_float, py_str, req_str, safe_int, str_default,
    str_or, truthy, Loose, Skip,
};
use crate::settings::ConsoleSettings;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde_json::{json, Map, Value};
use std::collections::{btree_map::Entry, BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use voice_core::xai::{XaiError, XaiRest};

const SESSION_SUFFIX: &str = "-session.json";
/// Turn phases that mean the agent is still working on a turn.
const IN_PROGRESS_PHASES: [&str; 4] = ["queued", "transcribed", "reasoned", "processing"];
/// Session statuses that are never shown as live.
const TERMINAL_STATUSES: [&str; 4] = ["completed", "transferred", "callback_requested", "failed"];
/// A WAV file with only its 44-byte header holds no audio.
const WAV_HEADER_BYTES: u64 = 44;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

fn other(err: impl std::fmt::Display) -> ServiceError {
    ServiceError::Other(err.to_string())
}

#[derive(Clone)]
pub struct SessionService {
    settings: Arc<ConsoleSettings>,
    xai: XaiRest,
}

// ---------------------------------------------------------------------------
// Small file and time helpers
// ---------------------------------------------------------------------------

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// `*.json` files in `dir` accepted by `keep` (given the file name), newest first.
fn json_files_newest_first(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<(PathBuf, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut files: Vec<(PathBuf, SystemTime)> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".json") && keep(name))
        })
        .filter_map(|entry| {
            let path = entry.path();
            mtime(&path).map(|modified| (path, modified))
        })
        .collect();
    files.sort_by_key(|file| std::cmp::Reverse(file.1));
    files
}

fn is_session_file(name: &str) -> bool {
    name.ends_with(SESSION_SUFFIX)
}

fn file_session_id(path: &Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.strip_suffix(SESSION_SUFFIX)
        .unwrap_or(name)
        .to_string()
}

/// UTC timestamp in the shape Python's `datetime.isoformat()` gives, rounded to the microsecond.
fn iso_from_system_time(time: SystemTime) -> String {
    let time: DateTime<Utc> = (time + std::time::Duration::from_nanos(500)).into();
    let format = if time.timestamp_subsec_micros() == 0 {
        "%Y-%m-%dT%H:%M:%S+00:00"
    } else {
        "%Y-%m-%dT%H:%M:%S%.6f+00:00"
    };
    time.format(format).to_string()
}

/// Parse an ISO-8601 timestamp. A timestamp without a timezone is taken as UTC.
pub fn parse_iso(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%d %H:%M:%S%.f%z"] {
        if let Ok(parsed) = DateTime::parse_from_str(value, format) {
            return Some(parsed.with_timezone(&Utc));
        }
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(value, format) {
            return Some(parsed.and_utc());
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc())
}

fn has_audio(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > WAV_HEADER_BYTES)
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|s| !s.is_empty())
}

/// `a or b` for an optional string and a plain string.
fn opt_or(value: &Option<String>, fallback: &str) -> String {
    non_empty(value).unwrap_or(fallback).to_string()
}

// ---------------------------------------------------------------------------
// Normalising the two on-disk turn shapes
// ---------------------------------------------------------------------------

/// A per-turn result file (`{job, reasoning, transcript, ...}`) as one interaction.
/// `None` when the payload does not have that shape.
pub fn normalize_telephony_payload(
    payload: &Value,
    raw_result_path: Option<&str>,
    observed_at: Option<&str>,
) -> Option<VoiceInResult> {
    telephony_payload(payload, raw_result_path, observed_at).ok()
}

fn telephony_payload(
    payload: &Value,
    raw_result_path: Option<&str>,
    observed_at: Option<&str>,
) -> Loose<VoiceInResult> {
    let payload = payload.as_object().ok_or(Skip)?;
    let empty = Map::new();
    let object_or_absent = |key: &str| -> Loose<&Map<String, Value>> {
        match payload.get(key) {
            None => Ok(&empty),
            Some(Value::Object(map)) => Ok(map),
            Some(_) => Err(Skip),
        }
    };
    let job = object_or_absent("job")?;
    let transcript_info = object_or_absent("transcript")?;
    let reasoning = match payload.get("reasoning") {
        Some(Value::Object(map)) => map,
        Some(other) if truthy(other) => return Err(Skip),
        _ => &empty,
    };

    let transcript = match get(reasoning, "transcript") {
        v if truthy(v) => req_str(v)?,
        _ => str_default(transcript_info, "text", "")?,
    };
    let slots = dict(reasoning, "slots");
    let action = match slots.map(|s| get(s, "action")) {
        Some(v) if truthy(v) => Some(py_str(v)),
        _ => Some(get(reasoning, "action"))
            .filter(|v| truthy(v))
            .map(py_str),
    };
    let timestamp = match or_chain(&[
        get(payload, "timestamp"),
        get(payload, "completed_at"),
        get(job, "timestamp"),
    ]) {
        v if truthy(v) => req_str(v)?,
        _ => observed_at.unwrap_or("").to_string(),
    };
    let turn = match get(payload, "turn") {
        Value::Null => get(job, "turn"),
        v => v,
    };
    let session_id = or_chain(&[
        get(payload, "conversation_id"),
        get(job, "conversation_id"),
        get(job, "call_id"),
    ]);

    Ok(VoiceInResult {
        source: "telephony".into(),
        request_id: str_default(job, "request_id", "unknown-request")?,
        timestamp,
        transcript,
        intent: str_default(reasoning, "intent", "")?,
        response: str_default(reasoning, "response", "")?,
        language: str_default(transcript_info, "language", "")?,
        duration_seconds: py_float(get(transcript_info, "duration"))?,
        provider: str_default(payload, "provider", "unknown")?,
        status: str_default(payload, "status", "")?,
        phase: str_default(payload, "phase", "")?,
        call_id: opt_str(get(job, "call_id"))?,
        session_id: opt_str(session_id)?,
        turn: safe_int(turn),
        input_audio_path: opt_str(get(job, "input_path"))?,
        output_audio_path: opt_str(get(job, "output_path"))?,
        raw_result_path: raw_result_path.map(str::to_string),
        observed_at: observed_at.map(str::to_string),
        target_number: opt_str(get(job, "to_user"))?,
        prompt_profile: opt_str(get(job, "prompt_profile"))?,
        action,
        slots: slots.cloned(),
        timings_ms: dict(payload, "timings_ms").cloned(),
    })
}

/// Where a turn inside a session file came from.
struct TurnContext<'a> {
    session_id: &'a str,
    observed_at: Option<&'a str>,
    raw_result_path: &'a str,
    target_number: &'a Value,
    prompt_profile: &'a Value,
}

/// One entry of a session file's `turns` list as an interaction.
fn session_turn(
    turn: &Map<String, Value>,
    fallback_turn: i64,
    ctx: &TurnContext,
) -> Loose<VoiceInResult> {
    let slots = dict(turn, "slots");
    let mut action = get(turn, "action");
    if !truthy(action) {
        if let Some(slots) = slots.filter(|s| !s.is_empty()) {
            action = get(slots, "action");
        }
    }
    let phase = str_or(get(turn, "phase"), "completed");
    let default_status = if IN_PROGRESS_PHASES.contains(&phase.as_str()) {
        "processing"
    } else {
        "completed"
    };
    Ok(VoiceInResult {
        source: "telephony".into(),
        request_id: str_default(turn, "request_id", "unknown-request")?,
        timestamp: ctx.observed_at.unwrap_or("").to_string(),
        transcript: str_default(turn, "transcript", "")?,
        intent: str_default(turn, "intent", "")?,
        response: str_default(turn, "response", "")?,
        language: String::new(),
        duration_seconds: 0.0,
        provider: "xai".into(),
        status: str_or(get(turn, "status"), default_status),
        phase,
        call_id: opt_str(get(turn, "call_id"))?,
        session_id: Some(ctx.session_id.to_string()),
        turn: Some(safe_int(get(turn, "turn")).unwrap_or(fallback_turn)),
        input_audio_path: None,
        output_audio_path: None,
        raw_result_path: Some(ctx.raw_result_path.to_string()),
        observed_at: ctx.observed_at.map(str::to_string),
        target_number: opt_str(or_chain(&[ctx.target_number, get(turn, "target_number")]))?,
        prompt_profile: opt_str(or_chain(&[ctx.prompt_profile, get(turn, "prompt_profile")]))?,
        action: Some(action).filter(|v| truthy(v)).map(py_str),
        slots: slots.cloned(),
        timings_ms: dict(turn, "timings_ms").cloned(),
    })
}

/// Keep one entry per turn number and drop repeated content.
///
/// For a repeated turn number a completed entry beats an unfinished one; otherwise
/// the later entry wins unless it carries less text. Entries with identical
/// transcript, intent, response and request id are collapsed. Turns without a
/// number get the next free one (turn 0, the greeting, is preserved), and the result
/// is ordered by turn number.
pub fn dedupe_session_turns(turns: Vec<VoiceInResult>) -> Vec<VoiceInResult> {
    let rank = |t: &VoiceInResult| u8::from(t.phase == "completed");
    let size = |t: &VoiceInResult| t.transcript.chars().count() + t.response.chars().count();
    let mut by_turn: BTreeMap<i64, VoiceInResult> = BTreeMap::new();
    let mut others: Vec<VoiceInResult> = Vec::new();
    for turn in turns {
        let Some(number) = turn.turn else {
            others.push(turn);
            continue;
        };
        match by_turn.entry(number) {
            Entry::Vacant(slot) => {
                slot.insert(turn);
            }
            Entry::Occupied(mut slot) => {
                let existing = slot.get();
                if rank(&turn) > rank(existing)
                    || (rank(&turn) == rank(existing) && size(&turn) >= size(existing))
                {
                    slot.insert(turn);
                }
            }
        }
    }

    let mut seen: HashSet<(String, String, String, String)> = HashSet::new();
    let mut deduped: Vec<VoiceInResult> = by_turn
        .into_values()
        .chain(others)
        .filter(|turn| {
            seen.insert((
                turn.transcript.trim().to_string(),
                turn.intent.trim().to_string(),
                turn.response.trim().to_string(),
                turn.request_id.trim().to_string(),
            ))
        })
        .collect();

    let mut next_fallback = 1;
    for index in 0..deduped.len() {
        if deduped[index].turn.is_some() {
            continue;
        }
        while deduped
            .iter()
            .enumerate()
            .any(|(other, t)| other != index && t.turn == Some(next_fallback))
        {
            next_fallback += 1;
        }
        deduped[index].turn = Some(next_fallback);
        next_fallback += 1;
    }
    deduped.sort_by_key(|t| t.turn.unwrap_or(9999));
    deduped
}

fn parse_post_analysis(raw: &Value) -> Option<CallPostAnalysis> {
    raw.as_object().filter(|m| !m.is_empty())?;
    serde_json::from_value(raw.clone()).ok()
}

fn parse_content_pack(raw: &Value) -> Option<CallContentPack> {
    raw.as_object().filter(|m| !m.is_empty())?;
    serde_json::from_value(raw.clone()).ok()
}

/// Group results by session, keeping first-seen order, each group sorted by turn then time.
fn group_results(results: Vec<VoiceInResult>) -> Vec<(String, Vec<VoiceInResult>)> {
    let mut groups: Vec<(String, Vec<VoiceInResult>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for result in results {
        let key = non_empty(&result.session_id)
            .or(non_empty(&result.call_id))
            .unwrap_or(&result.request_id)
            .to_string();
        let position = *index.entry(key.clone()).or_insert_with(|| {
            groups.push((key, Vec::new()));
            groups.len() - 1
        });
        groups[position].1.push(result);
    }
    for (_, turns) in &mut groups {
        turns.sort_by(|a, b| {
            let key =
                |t: &VoiceInResult| (t.turn.unwrap_or(9999), opt_or(&t.observed_at, &t.timestamp));
            key(a).cmp(&key(b))
        });
    }
    groups
}

/// Newest first by parsed time; timestamps that cannot be parsed sort after the
/// ones that can, by plain string comparison.
fn sort_newest_first(sessions: &mut [LiveCallSession]) {
    sessions.sort_by_cached_key(|session| {
        std::cmp::Reverse((parse_iso(&session.updated_at), session.updated_at.clone()))
    });
}

/// A compact string that changes whenever something the live view shows changes.
pub fn sessions_fingerprint(sessions: &[LiveCallSession]) -> String {
    sessions
        .iter()
        .map(|session| {
            let latest = session.turns.last();
            [
                session.session_id.as_str(),
                session.status.as_str(),
                session.updated_at.as_str(),
                session.latest_phase.as_str(),
                session.latest_transcript.as_str(),
                session.latest_response.as_str(),
                session.latest_action.as_deref().unwrap_or(""),
                &session.turn_count.to_string(),
                latest.map(|t| t.phase.as_str()).unwrap_or(""),
                latest.map(|t| t.request_id.as_str()).unwrap_or(""),
            ]
            .join("|")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// JSON schema for the structured reasoning answer on a device upload.
fn reasoning_schema() -> Value {
    json!({
        "type": "object",
        "title": "ReasoningResult",
        "properties": {
            "transcript": {
                "type": "string",
                "title": "Transcript",
                "description": "Verbatim or lightly normalized user utterance."
            },
            "intent": {
                "type": "string",
                "title": "Intent",
                "description": "Short machine-readable intent label such as greeting or ask_status."
            },
            "response": {
                "type": "string",
                "title": "Response",
                "description": "Natural language assistant response to speak back to the user."
            }
        },
        "required": ["transcript", "intent", "response"],
        "additionalProperties": false
    })
}

/// File extension (with the dot) for a saved upload; `.wav` when the name has none.
fn upload_suffix(filename: &str) -> String {
    let name = basename(filename);
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => name[dot..].to_string(),
        _ => ".wav".to_string(),
    }
}

impl SessionService {
    pub fn new(settings: ConsoleSettings, xai: XaiRest) -> Self {
        Self {
            settings: Arc::new(settings),
            xai,
        }
    }

    pub fn settings(&self) -> &ConsoleSettings {
        &self.settings
    }

    /// Creates artifacts_dir, artifacts_dir/uploads, artifacts_dir/results and brand_dir.
    pub fn ensure_dirs(&self) {
        let artifacts = &self.settings.artifacts_dir;
        for dir in [
            artifacts.clone(),
            artifacts.join("uploads"),
            artifacts.join("results"),
            self.settings.brand_dir.clone(),
        ] {
            let _ = std::fs::create_dir_all(dir);
        }
    }

    /// The client pack (`clients_dir/<id>/client.json`) as a JSON object; `{}` when
    /// it is missing or unreadable. `client_id` defaults to the configured client.
    pub fn load_client_pack(&self, client_id: Option<&str>) -> Value {
        let wanted = client_id
            .filter(|id| !id.is_empty())
            .or(Some(self.settings.default_client_id.as_str()).filter(|id| !id.is_empty()))
            .unwrap_or("sbr")
            .trim();
        let path = self.settings.clients_dir.join(wanted).join("client.json");
        match read_json(&path) {
            Some(Value::Object(pack)) => Value::Object(pack),
            _ => json!({}),
        }
    }

    /// The newest per-turn result files (not session files), newest first.
    pub fn list_recent_telephony_results(&self, limit: usize) -> Vec<VoiceInResult> {
        json_files_newest_first(&self.settings.telephony_results_dir, |name| {
            !is_session_file(name)
        })
        .into_iter()
        .take(limit)
        .filter_map(|(path, modified)| {
            let payload = read_json(&path)?;
            let observed_at = iso_from_system_time(modified);
            normalize_telephony_payload(&payload, Some(&path.to_string_lossy()), Some(&observed_at))
        })
        .collect()
    }

    fn load_session_files(&self, limit: usize) -> Vec<LiveCallSession> {
        json_files_newest_first(&self.settings.telephony_results_dir, is_session_file)
            .into_iter()
            .take(limit)
            .filter_map(|(path, modified)| self.session_from_file(&path, modified).ok().flatten())
            .collect()
    }

    /// `Ok(None)` for an ended call with nothing to show; `Err` for an unusable file.
    fn session_from_file(
        &self,
        path: &Path,
        modified: SystemTime,
    ) -> Loose<Option<LiveCallSession>> {
        let payload = read_json(path).ok_or(Skip)?;
        let payload = payload.as_object().ok_or(Skip)?;
        let observed_at = iso_from_system_time(modified);
        let file_id = file_session_id(path);
        let raw_session_id = str_or(get(payload, "conversation_id"), "")
            .trim()
            .to_string();
        // A session wiped by a race can persist conversation_id="unknown"; the file name
        // (or the first turn's call id) keeps one card per call.
        let mut session_id = if !raw_session_id.is_empty() && raw_session_id != "unknown" {
            raw_session_id
        } else {
            file_id
        };
        let target_number = or_chain(&[get(payload, "target_number"), get(payload, "to_user")]);
        let prompt_profile = get(payload, "prompt_profile");
        let status = match get(payload, "status") {
            v if truthy(v) => req_str(v)?,
            _ => "active".to_string(),
        };
        let slots = dict(payload, "slots").cloned().unwrap_or_default();
        let turns_raw: Vec<&Map<String, Value>> = match payload.get("turns") {
            None => vec![],
            Some(Value::Array(items)) => items
                .iter()
                .map(|t| t.as_object().ok_or(Skip))
                .collect::<Loose<_>>()?,
            Some(_) => return Err(Skip),
        };
        if session_id.is_empty() || session_id == "unknown" {
            let call_id = turns_raw
                .iter()
                .map(|turn| str_or(get(turn, "call_id"), "").trim().to_string())
                .find(|call_id| !call_id.is_empty() && call_id != "unknown");
            if let Some(call_id) = call_id {
                session_id = call_id;
            }
        }
        let raw_result_path = path.to_string_lossy();
        let ctx = TurnContext {
            session_id: &session_id,
            observed_at: Some(&observed_at),
            raw_result_path: &raw_result_path,
            target_number,
            prompt_profile,
        };
        let turns = turns_raw
            .iter()
            .zip(1i64..)
            .map(|(turn, index)| session_turn(turn, index, &ctx))
            .collect::<Loose<Vec<_>>>()?;
        let turns = dedupe_session_turns(turns);
        if turns.is_empty() && !truthy(get(payload, "latest_transcript")) && status != "active" {
            return Ok(None);
        }
        let latest = turns.last();
        let latest_str = |key: &str, from_turn: fn(&VoiceInResult) -> &str| -> String {
            match get(payload, key) {
                v if truthy(v) => py_str(v),
                _ => latest.map(from_turn).unwrap_or("").to_string(),
            }
        };
        let timestamp = |key: &str, fallback: Option<&str>| -> Loose<String> {
            match get(payload, key) {
                v if truthy(v) => req_str(v),
                _ => Ok(fallback
                    .filter(|f| !f.is_empty())
                    .unwrap_or(&observed_at)
                    .to_string()),
            }
        };
        let post_analysis = parse_post_analysis(get(payload, "post_analysis"));
        let channel_uuid = str_or(get(payload, "channel_uuid"), "");
        let mut recording_path = str_or(get(payload, "recording_path"), "");
        if recording_path.is_empty() && !channel_uuid.is_empty() {
            recording_path = format!("/var/lib/voice-ai/pilot/media/{channel_uuid}-call.wav");
        }
        let latest_action = match get(payload, "latest_action") {
            v if truthy(v) => Some(req_str(v)?),
            _ => match latest.and_then(|t| non_empty(&t.action)) {
                Some(action) => Some(action.to_string()),
                None if slots.is_empty() => None,
                None => match get(&slots, "action") {
                    Value::String(action) => Some(action.clone()),
                    v if truthy(v) => return Err(Skip),
                    _ => None,
                },
            },
        };
        let handoff_brief = match get(payload, "handoff_brief") {
            v if truthy(v) => py_str(v),
            _ => post_analysis
                .as_ref()
                .map(|a| a.internal_handoff_brief.clone())
                .unwrap_or_default(),
        };

        let mut session = LiveCallSession::new(&session_id);
        session.call_id = Some(
            latest
                .and_then(|t| non_empty(&t.call_id))
                .unwrap_or(&session_id)
                .to_string(),
        );
        session.started_at = timestamp(
            "started_at",
            turns.first().and_then(|t| t.observed_at.as_deref()),
        )?;
        session.updated_at = timestamp("updated_at", None)?;
        session.status = status;
        session.latest_intent = latest_str("latest_intent", |t| &t.intent);
        session.latest_transcript = latest_str("latest_transcript", |t| &t.transcript);
        session.latest_response = latest_str("latest_response", |t| &t.response);
        session.latest_action = latest_action;
        session.target_number = match latest.and_then(|t| non_empty(&t.target_number)) {
            Some(number) => Some(number.to_string()),
            None => opt_str(target_number)?,
        };
        session.prompt_profile = match latest.and_then(|t| non_empty(&t.prompt_profile)) {
            Some(profile) => Some(profile.to_string()),
            None => opt_str(prompt_profile)?,
        };
        session.slots = slots;
        session.latest_latency_ms = latest.and_then(|t| latency_ms(t.timings_ms.as_ref()));
        session.latest_phase = latest_str("latest_phase", |t| &t.phase);
        session.turn_count = turns.len() as i64;
        session.has_post_analysis = post_analysis.as_ref().is_some_and(|a| {
            a.status == "ready" && (!a.summary.is_empty() || !a.primary_intent.is_empty())
        });
        session.post_analysis = post_analysis;
        session.has_recording = self.recording_exists(&recording_path, &channel_uuid);
        session.recording_path = recording_path;
        session.channel_uuid = channel_uuid;
        session.handoff_brief = handoff_brief;
        session.content = parse_content_pack(get(payload, "content"));
        session.mode = str_or(get(payload, "mode"), "realtime");
        session.roleplay_scenario = str_or(get(payload, "roleplay_scenario"), "");
        session.roleplay_label = str_or(get(payload, "roleplay_label"), "");
        session.turns = turns;
        Ok(Some(session))
    }

    /// Sessions rebuilt from per-turn result files, for when no session file exists.
    fn sessions_from_results(&self, limit: usize) -> Vec<LiveCallSession> {
        group_results(self.list_recent_telephony_results(limit))
            .into_iter()
            .filter_map(|(session_id, turns)| {
                let turns = dedupe_session_turns(turns);
                let first = turns.first()?;
                let latest = turns.last()?;
                let mut session = LiveCallSession::new(&session_id);
                session.call_id = latest.call_id.clone();
                session.started_at = opt_or(&first.observed_at, &first.timestamp);
                session.updated_at = opt_or(&latest.observed_at, &latest.timestamp);
                session.status = if latest.status.is_empty() {
                    "active".into()
                } else {
                    latest.status.clone()
                };
                session.latest_intent = latest.intent.clone();
                session.latest_transcript = latest.transcript.clone();
                session.latest_response = latest.response.clone();
                session.latest_action = latest.action.clone();
                session.target_number = latest.target_number.clone();
                session.prompt_profile = latest.prompt_profile.clone();
                session.slots = latest.slots.clone().unwrap_or_default();
                session.latest_latency_ms = latency_ms(latest.timings_ms.as_ref());
                session.turn_count = turns.len() as i64;
                session.turns = turns;
                Some(session)
            })
            .collect()
    }

    /// Attach mid-turn progress (queued/transcribed/reasoned) from the newest result
    /// files onto the sessions, creating a live session for a call that has no
    /// session file yet.
    fn merge_in_progress_results(&self, sessions: &mut Vec<LiveCallSession>) {
        let results = self.list_recent_telephony_results(24);
        let mut by_id: HashMap<String, usize> = sessions
            .iter()
            .enumerate()
            .map(|(index, s)| (s.session_id.clone(), index))
            .collect();

        for (session_id, ordered) in group_results(results) {
            let Some(latest) = ordered.last().cloned() else {
                continue;
            };
            let in_progress_phase = IN_PROGRESS_PHASES.contains(&latest.phase.as_str());
            let Some(&index) = by_id.get(&session_id) else {
                if latest.status == "processing" || latest.status == "skipped" || in_progress_phase
                {
                    let seen_at = opt_or(&latest.observed_at, &latest.timestamp);
                    let mut session = LiveCallSession::new(&session_id);
                    session.call_id = latest.call_id.clone();
                    session.started_at = seen_at.clone();
                    session.updated_at = if seen_at.is_empty() {
                        utc_now_iso()
                    } else {
                        seen_at
                    };
                    session.status = "active".into();
                    session.latest_intent = latest.intent.clone();
                    session.latest_transcript = latest.transcript.clone();
                    session.latest_response = latest.response.clone();
                    session.latest_action = latest.action.clone();
                    session.target_number = latest.target_number.clone();
                    session.prompt_profile = latest.prompt_profile.clone();
                    session.slots = latest.slots.clone().unwrap_or_default();
                    session.latest_latency_ms = latency_ms(latest.timings_ms.as_ref());
                    session.latest_phase = if latest.phase.is_empty() {
                        latest.status.clone()
                    } else {
                        latest.phase.clone()
                    };
                    session.turn_count = ordered.len() as i64;
                    session.turns = ordered;
                    session.is_active = true;
                    by_id.insert(session_id, sessions.len());
                    sessions.push(session);
                }
                continue;
            };

            let session = &mut sessions[index];
            if !latest.phase.is_empty() {
                session.latest_phase = latest.phase.clone();
            } else if !latest.status.is_empty() {
                session.latest_phase = latest.status.clone();
            }
            // Only a completed turn that carries timings updates the latency.
            let completed_latency = (latest.phase == "completed"
                && latest.timings_ms.as_ref().is_some_and(|t| !t.is_empty()))
            .then(|| latency_ms(latest.timings_ms.as_ref()));
            if ["queued", "transcribed", "reasoned"].contains(&latest.phase.as_str())
                || latest.status == "processing"
            {
                if !latest.transcript.is_empty() {
                    session.latest_transcript = latest.transcript.clone();
                }
                if !latest.intent.is_empty() {
                    session.latest_intent = latest.intent.clone();
                }
                if !latest.response.is_empty() {
                    session.latest_response = latest.response.clone();
                }
                if let Some(action) = non_empty(&latest.action) {
                    session.latest_action = Some(action.to_string());
                }
                let seen_at = opt_or(&latest.observed_at, &latest.timestamp);
                if !seen_at.is_empty() {
                    session.updated_at = seen_at;
                }
                // Replace or append the in-progress turn for the timeline.
                if let Some(number) = latest.turn {
                    match session.turns.iter().position(|t| t.turn == Some(number)) {
                        Some(existing) => session.turns[existing] = latest,
                        None => {
                            let already_completed = session.turns.iter().any(|t| {
                                t.request_id == latest.request_id && t.phase == "completed"
                            });
                            if !already_completed {
                                session.turns.push(latest);
                                session.turns =
                                    dedupe_session_turns(std::mem::take(&mut session.turns));
                                session.turn_count = session.turns.len() as i64;
                            }
                        }
                    }
                }
            }
            if let Some(latency) = completed_latency {
                session.latest_latency_ms = latency;
            }
        }
    }

    /// The most recently updated calls, newest first, with `is_active` worked out.
    /// Reads files; call it from `spawn_blocking` in async code.
    pub fn list_live_call_sessions(&self, limit: usize) -> Vec<LiveCallSession> {
        let mut sessions = self.load_session_files(limit);
        if sessions.is_empty() {
            sessions = self.sessions_from_results(limit.saturating_mul(4));
        }
        self.merge_in_progress_results(&mut sessions);

        let now = Utc::now();
        for session in &mut sessions {
            let within_window = parse_iso(&session.updated_at).is_some_and(|seen| {
                (now - seen).num_milliseconds() <= self.settings.active_window_seconds * 1000
            });
            // An unfinished AI turn keeps the call live even when the session file is older.
            let in_progress = IN_PROGRESS_PHASES.contains(&session.latest_phase.as_str());
            session.is_active = (within_window || in_progress)
                && !TERMINAL_STATUSES.contains(&session.status.as_str());
            if session.latest_phase.is_empty() {
                if let Some(last) = session.turns.last() {
                    session.latest_phase = if last.phase.is_empty() {
                        "completed".into()
                    } else {
                        last.phase.clone()
                    };
                }
            }
        }
        sort_newest_first(&mut sessions);
        sessions.truncate(limit);
        sessions
    }

    /// The `*-session.json` file for an id: by file name, else by the `conversation_id`
    /// or a turn's `call_id` stored inside a file.
    fn session_file_path(&self, session_id: &str) -> Option<PathBuf> {
        let dir = &self.settings.telephony_results_dir;
        if session_id.is_empty() || !dir.is_dir() {
            return None;
        }
        // An id is never a path; refusing separators keeps lookups inside the results directory.
        if !session_id.contains(['/', '\\']) {
            let direct = dir.join(format!("{session_id}{SESSION_SUFFIX}"));
            if direct.exists() {
                return Some(direct);
            }
        }
        json_files_newest_first(dir, is_session_file)
            .into_iter()
            .map(|(path, _)| path)
            .find(|path| {
                if file_session_id(path) == session_id {
                    return true;
                }
                let Some(Value::Object(payload)) = read_json(path) else {
                    return false;
                };
                if str_or(get(&payload, "conversation_id"), "") == session_id {
                    return true;
                }
                get(&payload, "turns").as_array().is_some_and(|turns| {
                    turns
                        .iter()
                        .filter_map(Value::as_object)
                        .any(|turn| str_or(get(turn, "call_id"), "") == session_id)
                })
            })
    }

    /// A call by session id or call id. Looks at the 40 most recent calls first, then
    /// at older session files.
    pub fn get_session_by_id(&self, session_id: &str) -> Option<LiveCallSession> {
        if session_id.is_empty() {
            return None;
        }
        let matches = |s: &LiveCallSession| {
            s.session_id == session_id || s.call_id.as_deref() == Some(session_id)
        };
        if let Some(found) = self.list_live_call_sessions(40).into_iter().find(matches) {
            return Some(found);
        }
        // Not in the recent window: find its file, then the session loaded from it.
        let path = self.session_file_path(session_id)?;
        let file_id = file_session_id(&path);
        let from_file = |s: &LiveCallSession| {
            matches(s) || s.session_id == file_id || s.call_id.as_deref().unwrap_or("") == file_id
        };
        if let Some(found) = self.load_session_files(200).into_iter().find(from_file) {
            return Some(found);
        }
        self.minimal_session(&path, &file_id).ok()
    }

    /// A bare session straight from one file, for files the normal loader passes over.
    fn minimal_session(&self, path: &Path, file_id: &str) -> Loose<LiveCallSession> {
        let payload = read_json(path).ok_or(Skip)?;
        let payload = payload.as_object().ok_or(Skip)?;
        let conversation_id = str_or(get(payload, "conversation_id"), file_id);
        let raw_result_path = path.to_string_lossy();
        let ctx = TurnContext {
            session_id: &conversation_id,
            observed_at: None,
            raw_result_path: &raw_result_path,
            target_number: get(payload, "target_number"),
            prompt_profile: get(payload, "prompt_profile"),
        };
        let turns = get(payload, "turns")
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .zip(1i64..)
                    .filter_map(|(turn, index)| {
                        turn.as_object().map(|turn| session_turn(turn, index, &ctx))
                    })
                    .collect::<Loose<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let turns = dedupe_session_turns(turns);
        let latest = turns.last();
        let latest_str = |key: &str, from_turn: fn(&VoiceInResult) -> &str| -> String {
            match get(payload, key) {
                v if truthy(v) => py_str(v),
                _ => latest.map(from_turn).unwrap_or("").to_string(),
            }
        };
        let slots = dict(payload, "slots").cloned().unwrap_or_default();
        let post_analysis = parse_post_analysis(get(payload, "post_analysis"));

        let mut session = LiveCallSession::new(if conversation_id == "unknown" {
            file_id
        } else {
            &conversation_id
        });
        session.call_id = Some(
            latest
                .and_then(|t| non_empty(&t.call_id))
                .unwrap_or(file_id)
                .to_string(),
        );
        session.started_at = str_or(get(payload, "started_at"), "");
        session.updated_at = str_or(get(payload, "updated_at"), "");
        session.status = str_or(get(payload, "status"), "completed");
        session.latest_intent = latest_str("latest_intent", |t| &t.intent);
        session.latest_transcript = latest_str("latest_transcript", |t| &t.transcript);
        session.latest_response = latest_str("latest_response", |t| &t.response);
        session.latest_action = match latest.and_then(|t| non_empty(&t.action)) {
            Some(action) => Some(action.to_string()),
            None => opt_str(get(&slots, "action"))?,
        };
        session.target_number = opt_str(get(payload, "target_number"))?;
        session.prompt_profile = opt_str(get(payload, "prompt_profile"))?;
        session.slots = slots;
        session.latest_phase = str_or(get(payload, "latest_phase"), "");
        session.turn_count = turns.len() as i64;
        session.has_post_analysis = post_analysis
            .as_ref()
            .is_some_and(|a| a.status == "ready" && !a.summary.is_empty());
        session.post_analysis = post_analysis;
        session.mode = str_or(get(payload, "mode"), "realtime");
        session.roleplay_scenario = str_or(get(payload, "roleplay_scenario"), "");
        session.roleplay_label = str_or(get(payload, "roleplay_label"), "");
        session.turns = turns;
        Ok(session)
    }

    /// Map a recording path as the voice agent's container sees it onto this machine:
    /// the same file name in the media directory, else the path as given.
    fn host_media_path(&self, container_path: &str) -> Option<PathBuf> {
        if container_path.is_empty() {
            return None;
        }
        let media = &self.settings.telephony_media_dir;
        let candidate = media.join(basename(container_path));
        if candidate.exists() {
            return Some(candidate);
        }
        let direct = PathBuf::from(container_path);
        if direct.exists() {
            return Some(direct);
        }
        media.exists().then_some(candidate)
    }

    fn channel_recording(&self, channel_uuid: &str) -> Option<PathBuf> {
        if channel_uuid.is_empty() {
            return None;
        }
        Some(
            self.settings
                .telephony_media_dir
                .join(format!("{channel_uuid}-call.wav")),
        )
        .filter(|p| has_audio(p))
    }

    fn recording_exists(&self, recording_path: &str, channel_uuid: &str) -> bool {
        self.host_media_path(recording_path)
            .is_some_and(|p| has_audio(&p))
            || self.channel_recording(channel_uuid).is_some()
    }

    /// The call recording on this machine, when one with audio in it exists.
    pub fn resolve_recording_file(&self, session: &LiveCallSession) -> Option<PathBuf> {
        if let Some(path) = self
            .host_media_path(&session.recording_path)
            .filter(|p| has_audio(p))
        {
            return Some(path);
        }
        if let Some(path) = self.channel_recording(&session.channel_uuid) {
            return Some(path);
        }
        if session.channel_uuid.is_empty() {
            return None;
        }
        // Last resort: a recent recording whose name carries the channel id.
        let Ok(entries) = std::fs::read_dir(&self.settings.telephony_media_dir) else {
            return None;
        };
        let mut recordings: Vec<(PathBuf, SystemTime)> = entries
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with("-call.wav"))
            })
            .filter_map(|entry| mtime(&entry.path()).map(|modified| (entry.path(), modified)))
            .collect();
        recordings.sort_by_key(|file| std::cmp::Reverse(file.1));
        recordings
            .into_iter()
            .take(30)
            .map(|(path, _)| path)
            .find(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| name.contains(&session.channel_uuid))
                    && has_audio(path)
            })
    }

    /// Merge `fields` into the session's file and stamp `updated_at`. Written to a
    /// temporary file first, then renamed over the original.
    /// `Ok(None)` when the session has no readable file.
    pub fn persist_session_fields(
        &self,
        session_id: &str,
        fields: Map<String, Value>,
    ) -> Result<Option<PathBuf>, ServiceError> {
        let Some(path) = self.session_file_path(session_id) else {
            return Ok(None);
        };
        let Some(Value::Object(mut payload)) = read_json(&path) else {
            return Ok(None);
        };
        for (key, value) in fields {
            payload.insert(key, value);
        }
        payload.insert("updated_at".into(), json!(utc_now_iso()));
        let text = serde_json::to_string_pretty(&Value::Object(payload)).map_err(other)?;
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, text).map_err(other)?;
        std::fs::rename(&tmp, &path).map_err(other)?;
        Ok(Some(path))
    }

    /// Store an analysis in the session's file.
    pub fn persist_post_analysis(
        &self,
        session_id: &str,
        analysis: &CallPostAnalysis,
    ) -> Result<Option<PathBuf>, ServiceError> {
        let mut fields = Map::new();
        fields.insert(
            "post_analysis".into(),
            serde_json::to_value(analysis).map_err(other)?,
        );
        self.persist_session_fields(session_id, fields)
    }

    /// Ask the model for the post-call analysis. Falls back to the heuristic analysis
    /// (with `error` set to the reason) when the model cannot be used.
    pub async fn generate_post_analysis(&self, session: &LiveCallSession) -> CallPostAnalysis {
        let messages = vec![
            json!({"role": "system", "content": analysis_system_prompt(session)}),
            json!({"role": "user", "content": conversation_for_analysis(session)}),
        ];
        let outcome = match self.xai.chat_json(messages).await {
            Ok(content) => post_analysis_from_model(session, &content),
            // An empty answer is treated as an empty object, so every field falls back.
            Err(XaiError::Shape(_)) => post_analysis_from_model(session, "{}"),
            Err(err) => Err(err.to_string()),
        };
        outcome.unwrap_or_else(|error| {
            let mut analysis = heuristic_post_analysis(session);
            analysis.error = error;
            analysis
        })
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(SessionService) -> T + Send + 'static,
    ) -> Result<T, ServiceError> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || work(service))
            .await
            .map_err(other)
    }

    /// The session with its analysis and content pack, generating and storing whichever
    /// is missing. `force` regenerates both. `NotFound` when the session does not exist.
    pub async fn ensure_post_analysis(
        &self,
        session_id: &str,
        force: bool,
    ) -> Result<LiveCallSession, ServiceError> {
        let wanted = session_id.to_string();
        let mut session = self
            .blocking(move |service| service.get_session_by_id(&wanted))
            .await?
            .ok_or_else(|| ServiceError::NotFound(format!("Session not found: {session_id}")))?;
        let other_id = session
            .call_id
            .clone()
            .filter(|id| !id.is_empty() && *id != session.session_id);

        let stored = session
            .post_analysis
            .clone()
            .filter(|a| !force && a.status == "ready" && !a.summary.is_empty());
        let analysis = match stored {
            Some(analysis) => analysis,
            None => {
                let analysis = self.generate_post_analysis(&session).await;
                let (id, alias, to_store) = (
                    session.session_id.clone(),
                    other_id.clone(),
                    analysis.clone(),
                );
                self.blocking(move |service| -> Result<(), ServiceError> {
                    service.persist_post_analysis(&id, &to_store)?;
                    if let Some(alias) = alias {
                        service.persist_post_analysis(&alias, &to_store)?;
                    }
                    Ok(())
                })
                .await??;
                session.post_analysis = Some(analysis.clone());
                session.has_post_analysis = true;
                analysis
            }
        };

        // The content pack always exists afterwards (and is refreshed when forced).
        if force
            || session
                .content
                .as_ref()
                .is_none_or(|c| c.artifacts.is_empty())
        {
            let content = generate_call_content(&session, Some(&analysis));
            let content_json = serde_json::to_value(&content).map_err(other)?;
            let handoff_brief = if session.handoff_brief.is_empty() {
                &analysis.internal_handoff_brief
            } else {
                &session.handoff_brief
            };
            let mut fields = Map::new();
            fields.insert("content".into(), content_json.clone());
            fields.insert("handoff_brief".into(), json!(handoff_brief));
            let (id, alias) = (session.session_id.clone(), other_id);
            self.blocking(move |service| -> Result<(), ServiceError> {
                service.persist_session_fields(&id, fields)?;
                if let Some(alias) = alias {
                    let mut fields = Map::new();
                    fields.insert("content".into(), content_json);
                    service.persist_session_fields(&alias, fields)?;
                }
                Ok(())
            })
            .await??;
            session.content = Some(content);
        }
        if session.handoff_brief.trim().is_empty()
            && !analysis.internal_handoff_brief.trim().is_empty()
        {
            session.handoff_brief = analysis.internal_handoff_brief.clone();
        }
        session.has_recording =
            self.recording_exists(&session.recording_path, &session.channel_uuid);
        Ok(session)
    }

    /// Cheap change marker for the results directory: `"{count}:{newest mtime in ns}"`,
    /// or `"missing"`, `"empty"`, `"error"`.
    pub fn results_dir_signature(&self) -> String {
        let dir = &self.settings.telephony_results_dir;
        if !dir.exists() {
            return "missing".into();
        }
        let scan = || -> std::io::Result<(usize, u128)> {
            let (mut count, mut newest) = (0usize, 0u128);
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                if !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".json"))
                {
                    continue;
                }
                let modified = entry.metadata()?.modified()?;
                count += 1;
                newest = newest.max(
                    modified
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0),
                );
            }
            Ok((count, newest))
        };
        match scan() {
            Ok((0, _)) => "empty".into(),
            Ok((count, newest)) => format!("{count}:{newest}"),
            Err(_) => "error".into(),
        }
    }

    /// Handle an audio file sent from a device: save it, transcribe it, ask the model
    /// for `{transcript, intent, response}` and store the result as
    /// `artifacts_dir/results/<request_id>.json`.
    pub async fn process_device_upload(
        &self,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<DeviceSubmissionResult, ServiceError> {
        self.ensure_dirs();
        let artifacts = &self.settings.artifacts_dir;
        let upload_path = artifacts.join("uploads").join(format!(
            "{}{}",
            uuid::Uuid::new_v4(),
            upload_suffix(filename)
        ));
        tokio::fs::write(&upload_path, bytes).await.map_err(other)?;

        let request_id = uuid::Uuid::new_v4().to_string();
        let heard = self
            .xai
            .transcribe_file(&upload_path)
            .await
            .map_err(other)?;
        let timestamp = utc_now_iso();
        let messages = vec![
            json!({"role": "system", "content": DEVICE_SYSTEM_PROMPT}),
            json!({"role": "user", "content": heard.text}),
        ];
        let content = self
            .xai
            .chat_schema(messages, "ReasoningResult", reasoning_schema())
            .await
            .map_err(other)?;
        let reasoning: Value = serde_json::from_str(&content).map_err(other)?;
        let part = |key: &str| -> Result<String, ServiceError> {
            reasoning[key].as_str().map(str::to_string).ok_or_else(|| {
                ServiceError::Other(format!("Reasoning result is missing \"{key}\""))
            })
        };
        let (said, intent, response) = (part("transcript")?, part("intent")?, part("response")?);

        let upload_path_text = upload_path.to_string_lossy().to_string();
        let mut interaction = VoiceInResult::new(
            "device",
            &request_id,
            if said.is_empty() { &heard.text } else { &said },
            &intent,
            &response,
        );
        interaction.timestamp = timestamp.clone();
        interaction.language = heard.language.clone();
        interaction.duration_seconds = heard.duration;
        interaction.provider = "xai-stt".into();
        interaction.status = "completed".into();
        interaction.phase = "reasoned".into();
        interaction.input_audio_path = Some(upload_path_text.clone());
        interaction.observed_at = Some(timestamp.clone());

        let record = json!({
            "interaction": interaction,
            "transcript_event": {
                "request_id": request_id,
                "timestamp": timestamp,
                "transcript": heard.text,
                "language": heard.language,
                "duration_seconds": heard.duration,
                "provider": "xai-stt",
                "words": heard.words,
            },
            "reasoning_result": {"transcript": said, "intent": intent, "response": response},
        });
        let result_path = artifacts.join("results").join(format!("{request_id}.json"));
        let text = serde_json::to_string_pretty(&record).map_err(other)?;
        tokio::fs::write(&result_path, text).await.map_err(other)?;

        Ok(DeviceSubmissionResult {
            interaction,
            saved_upload_path: upload_path_text,
            saved_result_path: result_path.to_string_lossy().to_string(),
        })
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
