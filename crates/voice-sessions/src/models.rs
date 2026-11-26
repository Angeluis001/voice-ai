//! Data models served by the console APIs. Field names and defaults match the
//! original Python models, and every field is always serialized (`null` when absent)
//! because the browser code relies on that.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// UTC timestamp like `2026-11-23T01:02:03.123456+00:00`.
pub fn utc_now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

fn default_staff() -> String {
    "staff".into()
}
fn default_caller() -> String {
    "caller".into()
}
fn default_ready() -> String {
    "ready".into()
}
fn default_text() -> String {
    "text".into()
}
fn default_telephony() -> String {
    "telephony".into()
}
fn default_active() -> String {
    "active".into()
}
fn default_realtime() -> String {
    "realtime".into()
}

/// One interaction: a turn of a phone call, or a device upload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceInResult {
    pub source: String,
    pub request_id: String,
    #[serde(default = "utc_now_iso")]
    pub timestamp: String,
    pub transcript: String,
    pub intent: String,
    pub response: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub duration_seconds: f64,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub turn: Option<i64>,
    #[serde(default)]
    pub input_audio_path: Option<String>,
    #[serde(default)]
    pub output_audio_path: Option<String>,
    #[serde(default)]
    pub raw_result_path: Option<String>,
    #[serde(default)]
    pub observed_at: Option<String>,
    #[serde(default)]
    pub target_number: Option<String>,
    #[serde(default)]
    pub prompt_profile: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub slots: Option<Map<String, Value>>,
    #[serde(default)]
    pub timings_ms: Option<Map<String, Value>>,
}

impl VoiceInResult {
    pub fn new(
        source: &str,
        request_id: &str,
        transcript: &str,
        intent: &str,
        response: &str,
    ) -> Self {
        Self {
            source: source.into(),
            request_id: request_id.into(),
            timestamp: utc_now_iso(),
            transcript: transcript.into(),
            intent: intent.into(),
            response: response.into(),
            language: String::new(),
            duration_seconds: 0.0,
            provider: String::new(),
            status: String::new(),
            phase: String::new(),
            call_id: None,
            session_id: None,
            turn: None,
            input_audio_path: None,
            output_audio_path: None,
            raw_result_path: None,
            observed_at: None,
            target_number: None,
            prompt_profile: None,
            action: None,
            slots: None,
            timings_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceSubmissionResult {
    pub interaction: VoiceInResult,
    pub saved_upload_path: String,
    pub saved_result_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallActionItem {
    /// staff, caller, system or coach
    #[serde(default = "default_staff")]
    pub owner: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub due: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallStakeholder {
    #[serde(default = "default_caller")]
    pub role: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub notes: String,
}

/// Coaching scorecard for a roleplay call. Scores: overall 0-100, dimensions 0-10.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RoleplayScorecard {
    pub scenario: String,
    pub scenario_label: String,
    pub overall_score: i64,
    pub grade: String,
    pub greeting: i64,
    pub empathy: i64,
    pub discovery: i64,
    pub accuracy: i64,
    pub control: i64,
    pub strengths: Vec<String>,
    pub improvements: Vec<String>,
    pub coach_notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallPostAnalysis {
    /// ready, pending or failed
    #[serde(default = "default_ready")]
    pub status: String,
    #[serde(default)]
    pub primary_intent: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub caller_goal: String,
    #[serde(default)]
    pub key_facts: Map<String, Value>,
    #[serde(default)]
    pub recovered_slots: Map<String, Value>,
    #[serde(default)]
    pub turn_intents: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<CallActionItem>,
    #[serde(default)]
    pub next_best_action: String,
    #[serde(default)]
    pub stakeholders: Vec<CallStakeholder>,
    #[serde(default)]
    pub follow_up_email_draft: String,
    #[serde(default)]
    pub internal_handoff_brief: String,
    #[serde(default)]
    pub product_feedback: Vec<String>,
    #[serde(default)]
    pub roleplay_scorecard: Option<RoleplayScorecard>,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub error: String,
}

impl Default for CallPostAnalysis {
    fn default() -> Self {
        serde_json::from_value(Value::Object(Map::new())).expect("all fields have defaults")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallContentArtifact {
    pub key: String,
    pub title: String,
    pub body: String,
    /// text, email, sms or checklist
    #[serde(default = "default_text")]
    pub content_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallContentPack {
    #[serde(default = "default_ready")]
    pub status: String,
    #[serde(default)]
    pub artifacts: Vec<CallContentArtifact>,
    #[serde(default)]
    pub generated_at: String,
}

impl Default for CallContentPack {
    fn default() -> Self {
        Self {
            status: default_ready(),
            artifacts: vec![],
            generated_at: String::new(),
        }
    }
}

/// One phone call as the console shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LiveCallSession {
    pub session_id: String,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default = "default_telephony")]
    pub source: String,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub latest_intent: String,
    #[serde(default)]
    pub latest_transcript: String,
    #[serde(default)]
    pub latest_response: String,
    #[serde(default)]
    pub latest_action: Option<String>,
    #[serde(default)]
    pub latest_phase: String,
    #[serde(default = "default_active")]
    pub status: String,
    #[serde(default)]
    pub target_number: Option<String>,
    #[serde(default)]
    pub prompt_profile: Option<String>,
    #[serde(default)]
    pub slots: Map<String, Value>,
    #[serde(default)]
    pub turn_count: i64,
    #[serde(default)]
    pub latest_latency_ms: Option<i64>,
    #[serde(default)]
    pub turns: Vec<VoiceInResult>,
    #[serde(default)]
    pub recording_path: String,
    #[serde(default)]
    pub channel_uuid: String,
    #[serde(default)]
    pub handoff_brief: String,
    #[serde(default)]
    pub post_analysis: Option<CallPostAnalysis>,
    #[serde(default)]
    pub content: Option<CallContentPack>,
    #[serde(default)]
    pub has_post_analysis: bool,
    #[serde(default)]
    pub has_recording: bool,
    /// realtime, roleplay, or offline (ingested recordings)
    #[serde(default = "default_realtime")]
    pub mode: String,
    #[serde(default)]
    pub roleplay_scenario: String,
    #[serde(default)]
    pub roleplay_label: String,
}

impl LiveCallSession {
    pub fn new(session_id: &str) -> Self {
        serde_json::from_value(serde_json::json!({ "session_id": session_id })).expect("defaults")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_field_is_serialized_even_when_empty() {
        let value = serde_json::to_value(LiveCallSession::new("s1")).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj.len(), 28);
        assert_eq!(value["call_id"], Value::Null);
        assert_eq!(value["source"], "telephony");
        assert_eq!(value["status"], "active");
        assert_eq!(value["mode"], "realtime");
        assert_eq!(value["turns"], json!([]));
        assert_eq!(value["post_analysis"], Value::Null);
    }

    #[test]
    fn stored_analysis_parses_leniently() {
        let stored = json!({"summary": "ok", "action_items": [{"task": "call back"}], "roleplay_scorecard": {"overall_score": 80}});
        let a: CallPostAnalysis = serde_json::from_value(stored).unwrap();
        assert_eq!(a.status, "ready");
        assert_eq!(a.action_items[0].owner, "staff");
        assert_eq!(a.roleplay_scorecard.unwrap().overall_score, 80);
        assert_eq!(CallPostAnalysis::default().status, "ready");
    }

    #[test]
    fn timestamp_shape() {
        let t = utc_now_iso();
        assert!(t.ends_with("+00:00") && t.len() == 32, "{t}");
    }
}
