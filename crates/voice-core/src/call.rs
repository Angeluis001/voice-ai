//! The call engine: one live conversation between a caller and the Grok voice agent.
//!
//! The engine is transport-agnostic. A "leg" (a FreeSWITCH audio fork, or the PC's
//! microphone and speakers) feeds caller audio in as [`LegEvent`]s and receives
//! [`LegCommand`]s (AI audio to play, clear playout, transfer). All call state lives
//! in one task, so there are no locks around it.
use crate::{
    analysis::generate_post_call_analysis,
    audio::MixedCallRecorder,
    config::AgentSettings,
    intent::{reason_intent, HistoryTurn, IntentResult},
    pack::{ClientPack, PackLoader, RoleplayScenario, DEFAULT_GREETING, FALLBACK_SOP_INSTRUCTIONS},
    session::{turns, upsert_turn, SessionExtras, SessionMeta, SessionStore},
    slots::{merge_slots, normalize_action},
    util::{field, js_num, js_str, truthy},
    xai::{
        realtime::{self, RealtimeConfig, RealtimeEvent},
        XaiRest,
    },
    Doc,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};
use std::{collections::HashMap, collections::HashSet, path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::mpsc, time::Instant};

/// AI audio may still be playing from the far end's buffer this long after the last chunk.
const AUDIBLE_WINDOW: Duration = Duration::from_millis(4000);
const BARGE_DEBOUNCE: Duration = Duration::from_millis(300);
const BARGE_REENABLE: Duration = Duration::from_millis(600);
const BARGED_WINDOW: Duration = Duration::from_millis(8000);
/// Let the AI finish saying "I'll transfer you" before the call is bridged.
const TRANSFER_DELAY: Duration = Duration::from_millis(2500);
/// How long the end of a call waits for intent reasoning that is still running.
const INTENT_DRAIN: Duration = Duration::from_millis(4000);

/// Everything a call needs that outlives it.
#[derive(Clone)]
pub struct CallDeps {
    pub settings: Arc<AgentSettings>,
    pub store: Arc<SessionStore>,
    pub packs: Arc<PackLoader>,
    pub xai: XaiRest,
}

impl CallDeps {
    pub fn new(settings: AgentSettings) -> Self {
        let store = Arc::new(SessionStore::new(settings.result_dir.clone()));
        let packs = Arc::new(PackLoader::new(
            settings.clients_dir.clone(),
            &settings.default_client_id,
            &settings.xai.tts_voice,
        ));
        let xai = XaiRest::new(settings.xai.clone());
        Self {
            settings: Arc::new(settings),
            store,
            packs,
            xai,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CallParams {
    pub call_id: String,
    /// FreeSWITCH channel UUID; empty for a local microphone call.
    pub channel_uuid: String,
    pub target_number: String,
    pub roleplay: bool,
    pub client_id: String,
    pub scenario: String,
    /// PCM sample rate used on both the leg and the xAI session.
    pub sample_rate: u32,
    /// Whether a transfer request should be passed to the leg.
    pub can_transfer: bool,
}

impl CallParams {
    /// Parameters from the audio fork URL query
    /// (`call_id`, `uuid`, `target`, `mode`, `scenario`, `client_id`).
    pub fn from_query(query: &HashMap<String, String>, settings: &AgentSettings) -> Self {
        let get = |key: &str| query.get(key).filter(|v| !v.is_empty()).cloned();
        let call_id = get("call_id")
            .or_else(|| get("conversation_id"))
            .unwrap_or_else(|| format!("rt-{}", chrono::Utc::now().timestamp_millis()));
        let channel_uuid = get("uuid").unwrap_or_default();
        let target_number = get("target").unwrap_or_else(|| "9198".into());
        let mode = get("mode")
            .unwrap_or_else(|| "realtime".into())
            .to_lowercase();
        Self {
            roleplay: mode == "roleplay" || target_number == "9199",
            client_id: get("client_id")
                .or_else(|| get("client"))
                .unwrap_or_else(|| settings.default_client_id.clone()),
            scenario: get("scenario").unwrap_or_default(),
            sample_rate: settings.sample_rate,
            can_transfer: !channel_uuid.is_empty(),
            call_id,
            channel_uuid,
            target_number,
        }
    }
}

/// From the leg to the engine.
#[derive(Debug)]
pub enum LegEvent {
    /// Caller audio, raw PCM16LE mono at the call's sample rate.
    Audio(Vec<u8>),
    Closed,
    Error(String),
}

/// From the engine to the leg.
#[derive(Debug, PartialEq)]
pub enum LegCommand {
    /// AI audio to play to the caller, raw PCM16LE mono.
    Audio(Vec<u8>),
    /// The caller interrupted: drop any AI audio not yet played.
    ClearPlayout,
    /// Hand the caller to a human at this extension.
    Transfer { extension: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallOutcome {
    /// Why the call ended: `fork_closed`, `fork_error`, `xai_closed`, `xai_error`, `start_failed`.
    pub reason: String,
    pub session_path: PathBuf,
}

#[allow(clippy::large_enum_variant)]
enum Internal {
    IntentDone {
        turn: u64,
        user_text: String,
        assistant_text: String,
        slots_snapshot: Doc,
        result: Result<IntentResult, String>,
    },
    TransferDue,
}

/// Text handed to the human agent when a call is transferred.
pub fn build_live_handoff_brief(
    call_id: &str,
    intent: &str,
    slots: &Doc,
    user_text: &str,
    assistant_text: &str,
) -> String {
    let s = merge_slots(None, Some(&Value::Object(slots.clone())));
    let dash = |v: String| if v.is_empty() { "—".to_string() } else { v };
    let slot = |key: &str| match s.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    let action = if slot("action").is_empty() {
        "transfer".to_string()
    } else {
        slot("action")
    };
    [
        "INTERNAL HANDOFF BRIEF".to_string(),
        format!("Call: {}", dash(call_id.to_string())),
        format!("Intent: {}", dash(intent.to_string())),
        format!("Action: {}", dash(action)),
        format!("Debt type: {}", dash(slot("debt_type"))),
        format!("Amount: {}", dash(slot("amount"))),
        format!("Staff: {}", dash(slot("staff_name"))),
        format!("Reference: {}", dash(slot("reference"))),
        format!("Caller last said: {}", dash(user_text.to_string())),
        format!("AI last said: {}", dash(assistant_text.to_string())),
        "Please greet the caller and continue intake.".to_string(),
    ]
    .join("\n")
}

fn extract_transcript(event: &Value) -> String {
    let first = &event["item"]["content"][0];
    [
        &event["transcript"],
        &event["text"],
        &first["transcript"],
        &first["text"],
    ]
    .into_iter()
    .find(|v| truthy(v))
    .map(|v| js_str(Some(v)))
    .unwrap_or_default()
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn elapsed_ms(since: Option<Instant>) -> Option<u128> {
    since.map(|t| t.elapsed().as_millis())
}

fn within(since: Option<Instant>, window: Duration) -> bool {
    since.map(|t| t.elapsed() < window).unwrap_or(false)
}

struct Engine {
    deps: CallDeps,
    p: CallParams,
    pack: Arc<ClientPack>,
    session_path: PathBuf,
    meta: SessionMeta,
    xai_tx: Option<mpsc::Sender<Value>>,
    leg_tx: mpsc::Sender<LegCommand>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    recorder: MixedCallRecorder,

    turn_counter: u64,
    in_progress_turn: Option<u64>,
    user_partial: String,
    assistant: String,
    session_slots: Doc,
    last_live_write: Option<Instant>,

    // Barge-in state. A response can still be audible after its last audio chunk
    // while the far end drains its playout buffer.
    assistant_playing: bool,
    response_active: bool,
    last_assistant_audio: Option<Instant>,
    suppress_assistant_audio: bool,
    last_barge_in: Option<Instant>,

    transfer_in_flight: bool,
    pending_intents: usize,
    seen_event_types: HashSet<String>,
    audio_in: (u64, u64),
    audio_out: (u64, u64),
}

impl Engine {
    fn action(&self) -> String {
        let a = js_str(self.session_slots.get("action"));
        if a.is_empty() {
            "continue".into()
        } else {
            a
        }
    }

    fn turn_doc(
        &self,
        turn: u64,
        transcript: &str,
        intent: &str,
        response: &str,
        phase: &str,
        status: &str,
    ) -> Doc {
        let Value::Object(doc) = json!({
            "turn": turn,
            "transcript": transcript,
            "intent": intent,
            "response": response,
            "request_id": format!("{}-rt{turn}", self.p.call_id),
            "call_id": self.p.call_id,
            "target_number": self.p.target_number,
            "prompt_profile": "sop_enquiries_realtime",
            "action": self.action(),
            "slots": Value::Object(self.session_slots.clone()),
            "phase": phase,
            "status": status,
            "mode": "realtime",
        }) else {
            unreachable!()
        };
        doc
    }

    async fn patch<F: FnOnce(&mut Doc)>(&self, mutator: F) -> Option<Doc> {
        match self
            .deps
            .store
            .patch(&self.session_path, &self.meta, mutator)
            .await
        {
            Ok(doc) => Some(doc),
            Err(error) => {
                tracing::warn!(phase = "session_write_error", call_id = %self.p.call_id, %error);
                None
            }
        }
    }

    async fn send_xai(&self, event: Value) {
        if let Some(tx) = &self.xai_tx {
            let _ = tx.send(event).await;
        }
    }

    fn throttle(&mut self, min: Duration) -> bool {
        if within(self.last_live_write, min) {
            return false;
        }
        self.last_live_write = Some(Instant::now());
        true
    }

    fn ai_likely_audible(&self) -> bool {
        self.response_active
            || self.assistant_playing
            || within(self.last_assistant_audio, AUDIBLE_WINDOW)
    }

    async fn barge_in(&mut self, reason: &str) {
        if within(self.last_barge_in, BARGE_DEBOUNCE) {
            return;
        }
        self.last_barge_in = Some(Instant::now());
        let ai_was_active = self.ai_likely_audible();
        self.suppress_assistant_audio = true;
        self.assistant_playing = false;
        self.response_active = false;
        self.assistant.clear();
        self.send_xai(realtime::response_cancel()).await;
        let _ = self.leg_tx.send(LegCommand::ClearPlayout).await;
        tracing::info!(
            phase = "barge_in",
            call_id = %self.p.call_id,
            reason,
            ai_was_active,
            ms_since_audio = ?elapsed_ms(self.last_assistant_audio)
        );
    }

    async fn write_live_user(&mut self, text: &str, phase: &str) {
        if text.is_empty() {
            return;
        }
        let turn_no = *self.in_progress_turn.get_or_insert(self.turn_counter + 1);
        let turn = self.turn_doc(turn_no, text, "", "", phase, "processing");
        let slots = self.session_slots.clone();
        let (text, phase) = (text.to_string(), phase.to_string());
        self.patch(move |doc| {
            upsert_turn(doc, turn);
            doc.insert("status".into(), json!("active"));
            doc.insert("latest_phase".into(), json!(phase));
            doc.insert("latest_transcript".into(), json!(text));
            doc.insert("latest_intent".into(), json!(field(doc, "latest_intent")));
            doc.insert(
                "slots".into(),
                Value::Object(merge_slots(None, Some(&Value::Object(slots)))),
            );
        })
        .await;
    }

    async fn write_live_assistant(&mut self, text: &str, phase: &str) {
        if text.is_empty() {
            return;
        }
        // The greeting is already stored as turn 0; its live transcript is not a new turn.
        if self.turn_counter == 0 && self.in_progress_turn.is_none() && self.user_partial.is_empty()
        {
            return;
        }
        let turn_no = *self.in_progress_turn.get_or_insert(self.turn_counter + 1);
        let mut turn = self.turn_doc(turn_no, "", "", text, phase, "processing");
        turn.remove("intent");
        let slots = self.session_slots.clone();
        let user_partial = self.user_partial.clone();
        let (text, phase) = (text.to_string(), phase.to_string());
        self.patch(move |doc| {
            let existing = turns(doc)
                .into_iter()
                .find(|t| js_num(t.get("turn")) == turn_no as f64)
                .cloned();
            let existing = existing.unwrap_or_default();
            let transcript = [field(&existing, "transcript"), user_partial.clone()]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or_default();
            turn.insert("transcript".into(), json!(transcript));
            let request_id = field(&existing, "request_id");
            if !request_id.is_empty() {
                turn.insert("request_id".into(), json!(request_id));
            }
            upsert_turn(doc, turn);
            doc.insert("status".into(), json!("active"));
            doc.insert("latest_phase".into(), json!(phase));
            doc.insert("latest_response".into(), json!(text));
            if !user_partial.is_empty() {
                doc.insert("latest_transcript".into(), json!(user_partial));
            }
            doc.insert(
                "slots".into(),
                Value::Object(merge_slots(None, Some(&Value::Object(slots)))),
            );
        })
        .await;
    }

    async fn on_caller_audio(&mut self, pcm: Vec<u8>) {
        // Control frames from the fork arrive as JSON text; they are not audio.
        if pcm.len() < 2 || pcm[0] == b'{' {
            return;
        }
        self.recorder.push_caller(&pcm);
        self.audio_in.0 += 1;
        self.audio_in.1 += pcm.len() as u64;
        self.send_xai(realtime::audio_append(BASE64.encode(&pcm)))
            .await;
    }

    async fn on_ai_audio(&mut self, event: &Value) {
        if self.suppress_assistant_audio {
            return;
        }
        let encoded = [&event["delta"], &event["audio"]]
            .into_iter()
            .find_map(|v| v.as_str())
            .unwrap_or("");
        let Ok(mut pcm) = BASE64.decode(encoded) else {
            return;
        };
        if pcm.len() % 2 == 1 {
            pcm.pop();
        }
        if pcm.is_empty() {
            return;
        }
        if self
            .leg_tx
            .send(LegCommand::Audio(pcm.clone()))
            .await
            .is_err()
        {
            return;
        }
        // Record what the caller actually hears from the AI.
        self.recorder.push_ai(&pcm);
        self.assistant_playing = true;
        self.response_active = true;
        self.last_assistant_audio = Some(Instant::now());
        self.audio_out.0 += 1;
        self.audio_out.1 += pcm.len() as u64;
    }

    async fn on_xai_event(&mut self, event: Value) {
        let kind = event["type"].as_str().unwrap_or_default().to_string();
        if !kind.is_empty() && self.seen_event_types.insert(kind.clone()) {
            tracing::debug!(phase = "xai_event", call_id = %self.p.call_id, kind);
        }
        match kind.as_str() {
            "response.output_audio.delta" | "response.audio.delta" => {
                self.on_ai_audio(&event).await
            }
            "response.created" => {
                // A new response may play unless a barge-in has only just fired.
                if !within(self.last_barge_in, BARGE_REENABLE) {
                    self.suppress_assistant_audio = false;
                    self.response_active = true;
                }
            }
            "response.output_item.added" => {
                if !within(self.last_barge_in, BARGE_REENABLE) {
                    self.response_active = true;
                }
            }
            "input_audio_buffer.speech_started" => {
                if self.ai_likely_audible() {
                    self.barge_in("speech_started").await;
                } else {
                    tracing::debug!(phase = "speech_started_no_barge", call_id = %self.p.call_id);
                }
                self.in_progress_turn.get_or_insert(self.turn_counter + 1);
                self.patch(|doc| {
                    doc.insert("latest_phase".into(), json!("transcribed"));
                    doc.insert("status".into(), json!("active"));
                    if field(doc, "latest_transcript").is_empty() {
                        doc.insert("latest_transcript".into(), json!("…"));
                    }
                })
                .await;
            }
            "input_audio_buffer.speech_stopped" => {}
            "conversation.item.input_audio_transcription.updated"
            | "conversation.item.input_audio_transcription.completed"
            | "conversation.item.input_audio_transcription.done" => {
                let text = extract_transcript(&event).trim().to_string();
                if text.is_empty() {
                    return;
                }
                self.user_partial = text.clone();
                let is_final = !kind.ends_with(".updated");
                tracing::info!(
                    phase = if is_final { "user_transcript_final" } else { "user_transcript_live" },
                    call_id = %self.p.call_id,
                    text = %clip(&text, 120)
                );
                if is_final || self.throttle(Duration::from_millis(200)) {
                    self.write_live_user(&text, "transcribed").await;
                }
            }
            "response.output_audio_transcript.delta" | "response.audio_transcript.delta" => {
                self.assistant
                    .push_str(event["delta"].as_str().unwrap_or(""));
                if !self.assistant.is_empty() && self.throttle(Duration::from_millis(300)) {
                    let text = self.assistant.clone();
                    self.write_live_assistant(&text, "reasoned").await;
                }
            }
            "response.output_audio_transcript.done" | "response.audio_transcript.done" => {
                if let Some(full) = event["transcript"].as_str().filter(|t| !t.is_empty()) {
                    self.assistant = full.to_string();
                }
                if !self.assistant.is_empty() {
                    let text = self.assistant.clone();
                    self.write_live_assistant(&text, "reasoned").await;
                }
            }
            "response.done" | "response.completed" => self.on_response_done().await,
            "error" => {
                let detail = if event["error"].is_null() {
                    &event
                } else {
                    &event["error"]
                };
                tracing::warn!(phase = "xai_error", call_id = %self.p.call_id, error = %detail);
            }
            _ => {}
        }
    }

    /// A response finished: close the turn, then label intent in the background.
    async fn on_response_done(&mut self) {
        self.assistant_playing = false;
        self.response_active = false;
        // A response cancelled by barge-in leaves empty or partial assistant text.
        let was_barged = self.suppress_assistant_audio && within(self.last_barge_in, BARGED_WINDOW);
        if was_barged && self.assistant.trim().is_empty() {
            self.assistant.clear();
            return;
        }
        if !was_barged {
            self.suppress_assistant_audio = false;
        }

        let user_text = self.user_partial.trim().to_string();
        let assistant_text = self.assistant.trim().to_string();
        if user_text.is_empty() && assistant_text.is_empty() {
            self.user_partial.clear();
            self.assistant.clear();
            return;
        }
        // The forced greeting is already stored as turn 0.
        if user_text.is_empty() && self.turn_counter == 0 {
            self.user_partial.clear();
            self.assistant.clear();
            self.in_progress_turn = None;
            self.suppress_assistant_audio = false;
            return;
        }

        self.turn_counter += 1;
        let this_turn = self.in_progress_turn.take().unwrap_or(self.turn_counter);
        self.turn_counter = self.turn_counter.max(this_turn);

        // Persist the finished turn now so the console stays live.
        let placeholder = if user_text.is_empty() {
            "assistant_prompt"
        } else {
            "realtime_utterance"
        };
        let turn = self.turn_doc(
            this_turn,
            &user_text,
            placeholder,
            &assistant_text,
            "reasoned",
            "processing",
        );
        let (u, a) = (user_text.clone(), assistant_text.clone());
        self.patch(move |doc| {
            upsert_turn(doc, turn);
            doc.insert("latest_phase".into(), json!("reasoned"));
            doc.insert("latest_transcript".into(), json!(u));
            doc.insert("latest_response".into(), json!(a));
            doc.insert("status".into(), json!("active"));
        })
        .await;
        self.user_partial.clear();
        self.assistant.clear();

        let history: Vec<HistoryTurn> = match self.deps.store.read(&self.session_path).await {
            Some(doc) => turns(&doc)
                .into_iter()
                .filter(|t| {
                    js_num(t.get("turn")) < this_turn as f64 && field(t, "phase") == "completed"
                })
                .map(|t| HistoryTurn {
                    transcript: field(t, "transcript"),
                    response: field(t, "response"),
                })
                .collect(),
            None => vec![],
        };
        let slots_snapshot = self.session_slots.clone();
        let (xai, pack, tx) = (
            self.deps.xai.clone(),
            self.pack.clone(),
            self.internal_tx.clone(),
        );
        self.pending_intents += 1;
        tokio::spawn(async move {
            let result = reason_intent(
                &xai,
                &pack,
                &history,
                &user_text,
                &assistant_text,
                &slots_snapshot,
            )
            .await
            .map_err(|e| e.to_string());
            let _ = tx.send(Internal::IntentDone {
                turn: this_turn,
                user_text,
                assistant_text,
                slots_snapshot,
                result,
            });
        });
    }

    async fn on_intent_done(
        &mut self,
        turn: u64,
        user_text: String,
        assistant_text: String,
        slots_snapshot: Doc,
        result: Result<IntentResult, String>,
        call_live: bool,
    ) {
        self.pending_intents = self.pending_intents.saturating_sub(1);
        let mut intent = if user_text.is_empty() {
            "assistant_prompt"
        } else {
            "realtime_utterance"
        }
        .to_string();
        let mut merged = slots_snapshot.clone();
        match result {
            Ok(reasoned) => {
                if !reasoned.intent.is_empty() {
                    intent = reasoned.intent;
                }
                merged = merge_slots(
                    Some(&Value::Object(slots_snapshot)),
                    Some(&Value::Object(reasoned.slots)),
                );
                self.session_slots = merge_slots(
                    Some(&Value::Object(self.session_slots.clone())),
                    Some(&Value::Object(merged.clone())),
                );
            }
            Err(error) => tracing::warn!(phase = "intent_error", call_id = %self.p.call_id, %error),
        }

        let final_action = normalize_action(merged.get("action"));
        let handoff = if final_action == "transfer" {
            build_live_handoff_brief(
                &self.p.call_id,
                &intent,
                &merged,
                &user_text,
                &assistant_text,
            )
        } else {
            String::new()
        };
        let mut turn_doc = self.turn_doc(
            turn,
            &user_text,
            &intent,
            &assistant_text,
            "completed",
            "completed",
        );
        turn_doc.insert("action".into(), json!(final_action));
        turn_doc.insert("slots".into(), Value::Object(merged.clone()));
        let (u, a, i, act, m) = (
            user_text.clone(),
            assistant_text.clone(),
            intent.clone(),
            final_action.clone(),
            merged.clone(),
        );
        self.patch(move |doc| {
            upsert_turn(doc, turn_doc);
            doc.insert("latest_phase".into(), json!("completed"));
            doc.insert("latest_transcript".into(), json!(u));
            doc.insert("latest_response".into(), json!(a));
            doc.insert("latest_intent".into(), json!(i));
            doc.insert("latest_action".into(), json!(act));
            doc.insert(
                "slots".into(),
                Value::Object(merge_slots(doc.get("slots"), Some(&Value::Object(m)))),
            );
            if !handoff.is_empty() {
                doc.insert("handoff_brief".into(), json!(handoff));
                if !doc
                    .get("post_analysis")
                    .map(Value::is_object)
                    .unwrap_or(false)
                {
                    doc.insert("post_analysis".into(), json!({}));
                }
                doc["post_analysis"]["internal_handoff_brief"] = json!(handoff);
                doc["post_analysis"]["next_best_action"] = json!("transfer");
            }
            match act.as_str() {
                "transfer" => doc.insert("status".into(), json!("transferred")),
                "callback" => doc.insert("status".into(), json!("callback_requested")),
                _ => None,
            };
        })
        .await;
        tracing::info!(
            phase = "turn_complete",
            call_id = %self.p.call_id,
            turn,
            intent = %intent,
            action = %final_action,
            user = %clip(&user_text, 80),
            assistant = %clip(&assistant_text, 80)
        );

        // Hand over to a human once the AI has confirmed. Never during roleplay.
        if call_live
            && final_action == "transfer"
            && !self.transfer_in_flight
            && self.p.can_transfer
            && !self.p.roleplay
        {
            self.transfer_in_flight = true;
            let tx = self.internal_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(TRANSFER_DELAY).await;
                let _ = tx.send(Internal::TransferDue);
            });
        }
    }

    async fn on_transfer_due(&mut self) {
        self.send_xai(realtime::response_cancel()).await;
        self.suppress_assistant_audio = true;
        let extension = self.pack.transfer_extension();
        tracing::info!(phase = "transfer", call_id = %self.p.call_id, uuid = %self.p.channel_uuid, %extension);
        let _ = self.leg_tx.send(LegCommand::Transfer { extension }).await;
    }

    async fn on_internal(&mut self, message: Internal, call_live: bool) {
        match message {
            Internal::IntentDone {
                turn,
                user_text,
                assistant_text,
                slots_snapshot,
                result,
            } => {
                self.on_intent_done(
                    turn,
                    user_text,
                    assistant_text,
                    slots_snapshot,
                    result,
                    call_live,
                )
                .await
            }
            Internal::TransferDue if call_live => self.on_transfer_due().await,
            Internal::TransferDue => {}
        }
    }

    /// End of call: save the recording, set the final status, run post-call analysis.
    async fn cleanup(&mut self, reason: &str, internal_rx: &mut mpsc::UnboundedReceiver<Internal>) {
        tracing::info!(
            phase = "session_end",
            call_id = %self.p.call_id,
            reason,
            audio_in = self.audio_in.0,
            audio_out = self.audio_out.0,
            audio_bytes_in = self.audio_in.1,
            audio_bytes_out = self.audio_out.1
        );
        // Stop talking to xAI; dropping the sender closes the socket.
        self.xai_tx = None;

        // Let intent reasoning for the last turn(s) land so they are not left half-done.
        let deadline = Instant::now() + INTENT_DRAIN;
        while self.pending_intents > 0 {
            match tokio::time::timeout_at(deadline, internal_rx.recv()).await {
                Ok(Some(message)) => self.on_internal(message, false).await,
                _ => break,
            }
        }

        if let Some(rec) = self.recorder.finalize().await {
            tracing::info!(
                phase = "recording_saved",
                call_id = %self.p.call_id,
                path = %rec.path.display(),
                seconds = (rec.seconds * 10.0).round() / 10.0
            );
        }

        let slots = self.session_slots.clone();
        let final_doc = self
            .patch(move |doc| {
                let slot_action = slots.get("action").filter(|a| truthy(a)).cloned();
                let doc_action = doc.get("slots").and_then(|s| s.get("action")).cloned();
                match normalize_action(slot_action.or(doc_action).as_ref()).as_str() {
                    "transfer" => doc.insert("status".into(), json!("transferred")),
                    "callback" => doc.insert("status".into(), json!("callback_requested")),
                    _ if field(doc, "status") == "active" => {
                        doc.insert("status".into(), json!("completed"))
                    }
                    _ => None,
                };
                doc.insert("latest_phase".into(), json!("completed"));
                doc.insert(
                    "slots".into(),
                    Value::Object(merge_slots(None, Some(&Value::Object(slots)))),
                );
            })
            .await;

        if let Some(doc) = final_doc.filter(|d| !turns(d).is_empty()) {
            let analysis = generate_post_call_analysis(&self.deps.xai, &doc).await;
            tracing::info!(
                phase = "post_analysis",
                call_id = %self.p.call_id,
                primary_intent = %field(&analysis, "primary_intent"),
                outcome = %field(&analysis, "outcome"),
                summary = %clip(&field(&analysis, "summary"), 120)
            );
            self.patch(move |doc| {
                let recovered = analysis
                    .get("recovered_slots")
                    .filter(|s| truthy(s))
                    .cloned();
                if let Some(recovered) = recovered {
                    doc.insert(
                        "slots".into(),
                        Value::Object(merge_slots(doc.get("slots"), Some(&recovered))),
                    );
                }
                let primary = field(&analysis, "primary_intent");
                if !primary.is_empty() {
                    doc.insert("latest_intent".into(), json!(primary));
                }
                doc.insert("post_analysis".into(), Value::Object(analysis));
            })
            .await;
        }
        self.deps.store.forget(&self.session_path);
    }
}

/// Run one call to completion. Returns when the leg or xAI closes.
pub async fn run_call(
    deps: CallDeps,
    params: CallParams,
    mut leg_rx: mpsc::Receiver<LegEvent>,
    leg_tx: mpsc::Sender<LegCommand>,
) -> CallOutcome {
    let settings = deps.settings.clone();
    let pack = deps.packs.load(&params.client_id);
    let scenario = if params.scenario.is_empty() {
        let default = pack
            .raw
            .get("roleplay")
            .map(|r| js_str(r.get("default_scenario")))
            .unwrap_or_default();
        if default.is_empty() {
            "general".to_string()
        } else {
            default
        }
    } else {
        params.scenario.clone()
    };
    let rp: Option<RoleplayScenario> = params.roleplay.then(|| pack.roleplay(&scenario));
    let voice = [pack.tts_voice(), settings.xai.tts_voice.clone()]
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or_default();

    // Mixed recording (caller + AI): a channel recording alone only has the caller.
    let recording_stem = if params.channel_uuid.is_empty() {
        &params.call_id
    } else {
        &params.channel_uuid
    };
    let recording_path = settings
        .media_dir
        .join(format!("{recording_stem}-call.wav"));
    let recording_text = recording_path.to_string_lossy().replace('\\', "/");
    let extras = SessionExtras {
        recording_path: recording_text.clone(),
        channel_uuid: params.channel_uuid.clone(),
    };
    let meta = SessionMeta {
        call_id: params.call_id.clone(),
        target_number: params.target_number.clone(),
    };

    let (session_path, session_slots) = match deps
        .store
        .ensure(&params.call_id, &params.target_number, &extras)
        .await
    {
        Ok((path, doc)) => (path, merge_slots(None, doc.get("slots"))),
        Err(error) => {
            tracing::error!(phase = "session_failed", call_id = %params.call_id, %error);
            return CallOutcome {
                reason: "start_failed".into(),
                session_path: deps.store.path_for(&params.call_id),
            };
        }
    };

    let (internal_tx, mut internal_rx) = mpsc::unbounded_channel();
    let mut engine = Engine {
        recorder: MixedCallRecorder::new(recording_path, params.sample_rate),
        deps: deps.clone(),
        pack: pack.clone(),
        session_path: session_path.clone(),
        meta,
        xai_tx: None,
        leg_tx,
        internal_tx,
        turn_counter: 0,
        in_progress_turn: None,
        user_partial: String::new(),
        assistant: String::new(),
        session_slots,
        last_live_write: None,
        assistant_playing: false,
        response_active: false,
        last_assistant_audio: None,
        suppress_assistant_audio: false,
        last_barge_in: None,
        transfer_in_flight: false,
        pending_intents: 0,
        seen_event_types: HashSet::new(),
        audio_in: (0, 0),
        audio_out: (0, 0),
        p: params.clone(),
    };

    let mode = if params.roleplay {
        "roleplay"
    } else {
        "realtime"
    };
    {
        let (pack, rp, scenario, uuid, rec) = (
            pack.clone(),
            rp.clone(),
            scenario.clone(),
            params.channel_uuid.clone(),
            recording_text.clone(),
        );
        let client_id = if pack.id().is_empty() {
            params.client_id.clone()
        } else {
            pack.id()
        };
        engine
            .patch(move |doc| {
                doc.insert("recording_path".into(), json!(rec));
                let uuid = if uuid.is_empty() {
                    field(doc, "channel_uuid")
                } else {
                    uuid
                };
                doc.insert("channel_uuid".into(), json!(uuid));
                doc.insert("mode".into(), json!(mode));
                doc.insert("client_id".into(), json!(client_id));
                doc.insert("client_company".into(), json!(pack.company()));
                let profile = match &rp {
                    Some(_) => "roleplay".to_string(),
                    None => [
                        field(doc, "prompt_profile"),
                        pack.prompt_profile(),
                        "generic_receptionist".into(),
                    ]
                    .into_iter()
                    .find(|s| !s.is_empty())
                    .unwrap_or_default(),
                };
                doc.insert("prompt_profile".into(), json!(profile));
                if let Some(rp) = &rp {
                    doc.insert("roleplay_scenario".into(), json!(scenario));
                    doc.insert("roleplay_label".into(), json!(rp.label));
                    let track = if rp.track.is_empty() {
                        "enquiry"
                    } else {
                        &rp.track
                    };
                    doc.insert("roleplay_track".into(), json!(track));
                }
            })
            .await;
    }
    tracing::info!(
        phase = "call_connected",
        call_id = %params.call_id,
        uuid = %params.channel_uuid,
        sample_rate = params.sample_rate,
        mode,
        client_id = %params.client_id
    );

    let instructions = match &rp {
        Some(rp) => rp.instructions.clone(),
        None => {
            let sop = pack.sop_instructions();
            if sop.is_empty() {
                FALLBACK_SOP_INSTRUCTIONS.to_string()
            } else {
                sop
            }
        }
    };
    let config = RealtimeConfig {
        url: settings.xai.realtime_url.clone(),
        api_key: settings.xai.api_key.clone(),
        voice,
        instructions,
        sample_rate: params.sample_rate,
        vad: settings.vad.clone(),
    };
    let mut xai_rx = match realtime::connect(&config).await {
        Ok(conn) => {
            engine.xai_tx = Some(conn.tx);
            conn.rx
        }
        Err(error) => {
            tracing::error!(phase = "session_failed", call_id = %params.call_id, %error);
            engine.cleanup("start_failed", &mut internal_rx).await;
            return CallOutcome {
                reason: "start_failed".into(),
                session_path,
            };
        }
    };
    tracing::info!(phase = "xai_connected", call_id = %params.call_id, mode);

    let greeting = match &rp {
        Some(rp) => rp.opening.clone(),
        None => [
            pack.greeting(),
            settings.sop_greeting.clone().unwrap_or_default(),
            DEFAULT_GREETING.into(),
        ]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_default(),
    };
    engine.send_xai(realtime::force_message(&greeting)).await;
    {
        let opening_intent = if params.roleplay {
            "roleplay_customer_opening"
        } else {
            "opening_greeting"
        };
        let mut turn = engine.turn_doc(0, "", opening_intent, &greeting, "completed", "completed");
        turn.insert(
            "request_id".into(),
            json!(format!("{}-greeting", params.call_id)),
        );
        turn.insert(
            "prompt_profile".into(),
            json!(if params.roleplay {
                "roleplay"
            } else {
                "sop_enquiries_realtime"
            }),
        );
        turn.insert("action".into(), json!("continue"));
        turn.insert("mode".into(), json!(mode));
        let slots = engine.session_slots.clone();
        let greeting = greeting.clone();
        engine
            .patch(move |doc| {
                upsert_turn(doc, turn);
                doc.insert("latest_phase".into(), json!("completed"));
                doc.insert("latest_response".into(), json!(greeting));
                doc.insert("latest_intent".into(), json!(opening_intent));
                doc.insert("latest_transcript".into(), json!(""));
                doc.insert("slots".into(), Value::Object(slots));
                doc.insert("status".into(), json!("active"));
                doc.insert("mode".into(), json!(mode));
            })
            .await;
    }

    // Caller audio that arrived before the AI was ready is not replayed.
    let mut early_end = None;
    while let Ok(event) = leg_rx.try_recv() {
        match event {
            LegEvent::Audio(_) => {}
            LegEvent::Closed => early_end = Some("fork_closed"),
            LegEvent::Error(_) => early_end = Some("fork_error"),
        }
    }

    let reason = match early_end {
        Some(reason) => reason,
        None => loop {
            tokio::select! {
                leg = leg_rx.recv() => match leg {
                    Some(LegEvent::Audio(pcm)) => engine.on_caller_audio(pcm).await,
                    Some(LegEvent::Error(error)) => {
                        tracing::warn!(phase = "fork_error", call_id = %params.call_id, %error);
                        break "fork_error";
                    }
                    Some(LegEvent::Closed) | None => break "fork_closed",
                },
                xai = xai_rx.recv() => match xai {
                    Some(RealtimeEvent::Event(event)) => engine.on_xai_event(event).await,
                    Some(RealtimeEvent::Error(error)) => {
                        tracing::warn!(phase = "xai_ws_error", call_id = %params.call_id, %error);
                        break "xai_error";
                    }
                    Some(RealtimeEvent::Closed) | None => break "xai_closed",
                },
                Some(message) = internal_rx.recv() => engine.on_internal(message, true).await,
            }
        },
    };

    engine.cleanup(reason, &mut internal_rx).await;
    CallOutcome {
        reason: reason.into(),
        session_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::empty_slots;

    #[test]
    fn handoff_brief_layout() {
        let mut slots = empty_slots();
        slots.insert("debt_type".into(), json!("invoice"));
        slots.insert("action".into(), json!("transfer"));
        let brief =
            build_live_handoff_brief("c1", "request_transfer", &slots, "a person please", "");
        assert_eq!(
            brief,
            "INTERNAL HANDOFF BRIEF\nCall: c1\nIntent: request_transfer\nAction: transfer\nDebt type: invoice\nAmount: —\nStaff: —\nReference: —\nCaller last said: a person please\nAI last said: —\nPlease greet the caller and continue intake."
        );
    }

    #[test]
    fn transcript_is_found_in_any_known_field() {
        assert_eq!(extract_transcript(&json!({"transcript": "a"})), "a");
        assert_eq!(extract_transcript(&json!({"text": "b"})), "b");
        assert_eq!(
            extract_transcript(&json!({"item": {"content": [{"transcript": "c"}]}})),
            "c"
        );
        assert_eq!(
            extract_transcript(&json!({"item": {"content": [{"text": "d"}]}})),
            "d"
        );
        assert_eq!(extract_transcript(&json!({})), "");
    }

    #[test]
    fn params_from_fork_query() {
        let settings = AgentSettings::from_env();
        let q = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let p = CallParams::from_query(
            &q(&[("call_id", "abc"), ("uuid", "u-1"), ("target", "9198")]),
            &settings,
        );
        assert_eq!(
            (
                p.call_id.as_str(),
                p.channel_uuid.as_str(),
                p.roleplay,
                p.can_transfer
            ),
            ("abc", "u-1", false, true)
        );
        let rp = CallParams::from_query(
            &q(&[
                ("target", "9199"),
                ("mode", "ROLEPLAY"),
                ("scenario", "fees"),
            ]),
            &settings,
        );
        assert!(rp.roleplay && rp.call_id.starts_with("rt-") && !rp.can_transfer);
        assert_eq!(rp.scenario, "fees");
        let by_target = CallParams::from_query(&q(&[("target", "9199")]), &settings);
        assert!(by_target.roleplay);
    }
}
