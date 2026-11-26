//! Post-call analysis: an xAI summary when a key is available, otherwise (or on any
//! failure) a rule-based summary. Roleplay calls also get a coaching scorecard.
use crate::{
    session::turns,
    slots::{doc_slots, merge_slots, normalize_action},
    util::{field, js_num, js_str, now_iso, truthy},
    xai::XaiRest,
    Doc,
};
use regex::Regex;
use serde_json::{json, Value};

const SYSTEM_INTENTS: [&str; 4] = [
    "opening_greeting",
    "assistant_prompt",
    "realtime_utterance",
    "roleplay_customer_opening",
];

/// A roleplay (training) call: the AI plays the customer and staff play the agent.
pub fn is_roleplay_doc(doc: &Doc) -> bool {
    field(doc, "mode").to_lowercase() == "roleplay"
        || field(doc, "prompt_profile").to_lowercase() == "roleplay"
        || field(doc, "target_number") == "9199"
}

fn is_greeting_turn(turn: &Doc) -> bool {
    let intent = field(turn, "intent");
    intent == "opening_greeting"
        || intent == "roleplay_customer_opening"
        || (js_num(turn.get("turn")) == 0.0
            && !field(turn, "response").is_empty()
            && field(turn, "transcript").is_empty())
}

fn turn_label(turn: &Doc) -> String {
    match turn.get("turn") {
        Some(Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => String::new(),
    }
}

/// The call as plain text for the analysis prompt.
pub fn build_conversation_text(doc: &Doc) -> String {
    let roleplay = is_roleplay_doc(doc);
    let or = |value: String, fallback: &str| {
        if value.is_empty() {
            fallback.to_string()
        } else {
            value
        }
    };
    let mut lines = vec![
        format!("Session: {}", or(field(doc, "conversation_id"), "unknown")),
        format!(
            "Line: {} · status={}",
            or(field(doc, "target_number"), "—"),
            field(doc, "status")
        ),
    ];
    if roleplay {
        lines.push("MODE: ROLEPLAY — Staff = receptionist trainee; AI = customer persona".into());
        lines.push(format!(
            "Scenario: {} ({})",
            or(field(doc, "roleplay_scenario"), "invoice"),
            field(doc, "roleplay_label")
        ));
    }
    let slots = doc
        .get("slots")
        .filter(|s| truthy(s))
        .cloned()
        .unwrap_or_else(|| json!({}));
    lines.push(format!("Known slots: {slots}"));
    lines.push("Transcript:".into());
    for turn in turns(doc) {
        let (transcript, response) = (field(turn, "transcript"), field(turn, "response"));
        if is_greeting_turn(turn) {
            let tag = if roleplay {
                "[AI customer opening]"
            } else {
                "[AI greeting]"
            };
            lines.push(format!("{tag} {response}"));
            continue;
        }
        let n = turn_label(turn);
        if !transcript.is_empty() {
            let who = if roleplay { "Staff" } else { "Caller" };
            lines.push(format!("[{who} T{n}] {transcript}"));
        }
        if !response.is_empty() {
            let intent = field(turn, "intent");
            let intent_bit = if intent.is_empty() {
                String::new()
            } else {
                format!(" (intent={intent})")
            };
            let who = if roleplay { "AI customer" } else { "AI" };
            lines.push(format!("[{who} T{n}]{intent_bit} {response}"));
        }
    }
    lines.join("\n")
}

fn caller_bits(doc: &Doc) -> Vec<String> {
    turns(doc)
        .iter()
        .map(|t| field(t, "transcript").trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn matches(pattern: &str, text: &str) -> bool {
    Regex::new(pattern).expect("static pattern").is_match(text)
}

/// Rule-based coaching scorecard from what staff said during a roleplay call.
pub fn heuristic_roleplay_scorecard(doc: &Doc) -> Doc {
    let scenario = {
        let s = field(doc, "roleplay_scenario");
        if s.is_empty() {
            "invoice".to_string()
        } else {
            s
        }
    };
    let label = {
        let l = field(doc, "roleplay_label");
        if l.is_empty() {
            scenario.clone()
        } else {
            l
        }
    };
    let staff_bits: Vec<String> = caller_bits(doc).iter().map(|s| s.to_lowercase()).collect();
    let joined = staff_bits.join(" ");
    let slots = doc_slots(doc);
    let has = |key: &str| slots.get(key).map(truthy).unwrap_or(false);

    let mut greeting = if staff_bits.is_empty() { 2 } else { 6 };
    if matches(
        r"(good morning|hello|hi |slater|how can i|speaking)",
        &joined,
    ) {
        greeting = 8;
    }

    let mut empathy = 4;
    if matches(r"(sorry|understand|frustrat|hear you|appreciate)", &joined) {
        empathy = 8;
    } else if scenario == "angry" {
        empathy = 3;
    }

    let mut discovery = 3;
    if has("debt_type") || matches(r"(debt|invoice|type of|what kind)", &joined) {
        discovery += 3;
    }
    if has("amount") || matches(r"(amount|how much|dollar|\$)", &joined) {
        discovery += 3;
    }
    let discovery = discovery.min(10);

    let accuracy = match scenario.as_str() {
        "fees" => {
            if matches(
                r"(no joining|no fee|commission|only when|successfully)",
                &joined,
            ) {
                9
            } else if matches(r"(fee|cost|charge)", &joined) {
                6
            } else {
                3
            }
        }
        "unsupported" => {
            if matches(
                r"(cannot|can't|unable|not able|don't take|letter of demand|template)",
                &joined,
            ) {
                9
            } else {
                3
            }
        }
        "transfer" => {
            if matches(
                r"(transfer|put you through|connect you|colleague|person)",
                &joined,
            ) {
                8
            } else {
                4
            }
        }
        _ => 5,
    };

    let mut control: i64 = if staff_bits.len() >= 2 { 6 } else { 4 };
    if joined.encode_utf16().count() > 800 {
        control = (control - 2).max(3);
    }

    let dims = [greeting, empathy, discovery, accuracy, control];
    let mean = dims.iter().sum::<i64>() as f64 / dims.len() as f64;
    let overall = (mean * 10.0 + 0.5).floor() as i64;
    let grade = match overall {
        85.. => "A",
        70..=84 => "B",
        55..=69 => "C",
        40..=54 => "D",
        _ => "F",
    };

    let mut strengths = vec![];
    let mut improvements = vec![];
    if greeting >= 7 {
        strengths.push("Professional opening");
    } else {
        improvements.push("Open with a clear company greeting");
    }
    if discovery >= 7 {
        strengths.push("Gathered key enquiry details");
    } else {
        improvements.push("Ask debt type and amount early");
    }
    if accuracy >= 7 {
        strengths.push("SOP facts landed correctly");
    } else {
        improvements.push("Review SOP for fees / unsupported / transfer language");
    }
    if empathy >= 7 {
        strengths.push("Showed empathy");
    } else if scenario == "angry" || scenario == "transfer" {
        improvements.push("Acknowledge frustration before process questions");
    }
    if strengths.is_empty() {
        strengths.push("Completed practice call");
    }
    if improvements.is_empty() {
        improvements.push("Keep practicing structured intake");
    }

    let Value::Object(card) = json!({
        "scenario": scenario,
        "scenario_label": label,
        "overall_score": overall,
        "grade": grade,
        "greeting": greeting,
        "empathy": empathy,
        "discovery": discovery,
        "accuracy": accuracy,
        "control": control,
        "strengths": strengths,
        "improvements": improvements,
        "coach_notes": format!("Practice scenario: {label}. Offline score from transcript heuristics."),
    }) else {
        unreachable!()
    };
    card
}

fn preview(bits: &[String]) -> String {
    let head: Vec<&str> = bits.iter().take(3).map(String::as_str).collect();
    format!(
        "{}{}",
        head.join(" · "),
        if bits.len() > 3 { "…" } else { "" }
    )
}

fn join_list(card: &Doc, key: &str) -> String {
    let joined = card
        .get(key)
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .map(|v| js_str(Some(v)))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if joined.is_empty() {
        "—".into()
    } else {
        joined
    }
}

/// Rule-based post-call analysis. Always succeeds.
pub fn heuristic_post_analysis(doc: &Doc) -> Doc {
    let roleplay = is_roleplay_doc(doc);
    let slots = doc_slots(doc);
    let slot_action = slots
        .get("action")
        .filter(|a| truthy(a))
        .or_else(|| doc.get("latest_action"));
    let action = normalize_action(slot_action);
    let status = field(doc, "status");
    let all_turns = turns(doc);
    let intents: Vec<String> = all_turns
        .iter()
        .map(|t| field(t, "intent"))
        .filter(|i| !i.is_empty() && !SYSTEM_INTENTS.contains(&i.as_str()))
        .collect();

    let mut primary = field(doc, "latest_intent");
    if primary.is_empty() {
        primary = intents.last().cloned().unwrap_or_else(|| "unknown".into());
    }
    let scenario = field(doc, "roleplay_scenario");
    let label = field(doc, "roleplay_label");
    let mut outcome = "ended";
    if roleplay {
        outcome = if all_turns.len() > 1 {
            "practice_completed"
        } else {
            "practice_partial"
        };
        primary = format!(
            "roleplay_{}",
            if scenario.is_empty() {
                "invoice"
            } else {
                &scenario
            }
        );
    } else if action == "transfer" || status == "transferred" {
        outcome = "transferred";
        if primary == "unknown" {
            primary = "request_transfer".into();
        }
    } else if action == "callback" || status == "callback_requested" {
        outcome = "callback_requested";
    } else if action == "decline_debt_type" {
        outcome = "declined_unsupported";
    } else if action == "complete" || status == "completed" {
        outcome = "completed";
    }

    let bits = caller_bits(doc);
    let practice = [label.clone(), scenario.clone()]
        .into_iter()
        .find(|s| !s.is_empty());
    let mut parts = vec![];
    if roleplay {
        parts.push(format!(
            "Roleplay training ({}).",
            practice.clone().unwrap_or_else(|| "practice".into())
        ));
        if !bits.is_empty() {
            parts.push(format!("Staff said: {}.", preview(&bits)));
        }
    } else if !bits.is_empty() {
        parts.push(format!("Caller discussed: {}.", preview(&bits)));
    }
    let slot_text = |key: &str| js_str(slots.get(key));
    if !slot_text("debt_type").is_empty() {
        parts.push(format!("Debt type noted as {}.", slot_text("debt_type")));
    }
    if !slot_text("amount").is_empty() {
        parts.push(format!("Amount noted as {}.", slot_text("amount")));
    }
    parts.push(format!("Outcome: {}.", outcome.replace('_', " ")));

    let mut key_facts = Doc::new();
    for key in ["debt_type", "amount", "staff_name", "reference", "action"] {
        if let Some(value) = slots.get(key) {
            if !value.is_null() && value.as_str() != Some("") {
                key_facts.insert(key.into(), value.clone());
            }
        }
    }
    if roleplay && !scenario.is_empty() {
        key_facts.insert("roleplay_scenario".into(), json!(scenario));
    }

    let scorecard = roleplay.then(|| heuristic_roleplay_scorecard(doc));
    let handoff = scorecard
        .as_ref()
        .map(|card| {
            [
                "ROLEPLAY SCORECARD".to_string(),
                format!("Scenario: {}", field(card, "scenario_label")),
                format!(
                    "Score: {}/100 ({})",
                    card["overall_score"],
                    field(card, "grade")
                ),
                format!("Strengths: {}", join_list(card, "strengths")),
                format!("Improvements: {}", join_list(card, "improvements")),
                field(card, "coach_notes"),
            ]
            .join("\n")
        })
        .unwrap_or_default();

    let caller_goal = if roleplay {
        format!(
            "Practice scenario: {}",
            practice.unwrap_or_else(|| "invoice".into())
        )
    } else {
        bits.first()
            .cloned()
            .unwrap_or_else(|| "Not clearly captured".into())
    };
    let action_items = if roleplay {
        json!([{"owner": "staff", "task": "Review roleplay scorecard and retry weak scenarios", "due": "same_day"}])
    } else {
        json!([])
    };

    let Value::Object(mut analysis) = json!({
        "status": "ready",
        "primary_intent": primary,
        "summary": parts.join(" "),
        "outcome": outcome,
        "caller_goal": caller_goal,
        "key_facts": key_facts,
        "recovered_slots": slots,
        "turn_intents": intents,
        "risks": [],
        "action_items": action_items,
        "next_best_action": if roleplay { "follow_up".to_string() } else { action },
        "internal_handoff_brief": handoff,
    }) else {
        unreachable!()
    };
    if let Some(card) = scorecard {
        analysis.insert("roleplay_scorecard".into(), Value::Object(card));
    }
    analysis.insert("generated_at".into(), json!(now_iso()));
    analysis.insert("error".into(), json!(""));
    analysis
}

fn analysis_prompt(roleplay: bool, company: &str) -> String {
    if roleplay {
        format!(
            "You are a call-quality coach for ROLEPLAY training at {company}.
AI = CUSTOMER persona; Staff = receptionist trainee.
Return strict JSON only with:
- primary_intent: roleplay_<scenario>
- summary: 2-4 sentences on staff performance
- outcome: practice_completed|practice_partial|practice_abandoned
- caller_goal: what AI customer wanted
- key_facts: object of real facts staff collected
- recovered_slots: {{debt_type, amount, staff_name, reference, escalation_offered, action}}
- roleplay_scorecard: {{
    scenario, scenario_label, overall_score (0-100), grade (A|B|C|D|F),
    greeting, empathy, discovery, accuracy, control (each 0-10),
    strengths: string[], improvements: string[], coach_notes: string
  }}
Score honestly from transcript only. Do not invent."
        )
    } else {
        "You are a post-call analyst for a debt recovery enquiries phone line.
Return strict JSON only with:
- primary_intent: snake_case overall call purpose
- summary: 2-4 sentence operator summary
- outcome: transferred|callback_requested|enquiry_in_progress|declined_unsupported|completed|abandoned|ended
- caller_goal: one sentence
- key_facts: object of real facts only
- recovered_slots: {debt_type, amount, staff_name, reference, escalation_offered, action}
Do not invent facts."
            .to_string()
    }
}

fn build_from_model(doc: &Doc, parsed: &Value, fallback: Doc) -> Doc {
    let roleplay = is_roleplay_doc(doc);
    let slots = merge_slots(doc.get("slots"), parsed.get("recovered_slots"));
    let intents: Vec<String> = turns(doc)
        .iter()
        .map(|t| field(t, "intent"))
        .filter(|i| !i.is_empty() && i != "opening_greeting" && i != "roleplay_customer_opening")
        .collect();
    let pick = |candidates: [String; 3]| {
        candidates
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_default()
    };
    let text = |key: &str| js_str(parsed.get(key));

    let scorecard = roleplay.then(|| {
        let mut card = fallback
            .get("roleplay_scorecard")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(Value::Object(model_card)) = parsed.get("roleplay_scorecard") {
            for (k, v) in model_card {
                card.insert(k.clone(), v.clone());
            }
            let scenario = pick([
                js_str(model_card.get("scenario")),
                field(doc, "roleplay_scenario"),
                "invoice".into(),
            ]);
            let label = pick([
                js_str(model_card.get("scenario_label")),
                field(doc, "roleplay_label"),
                String::new(),
            ]);
            card.insert("scenario".into(), json!(scenario));
            card.insert("scenario_label".into(), json!(label));
        }
        card
    });

    let summary = text("summary").trim().to_string();
    let caller_goal = text("caller_goal").trim().to_string();
    let slot_action = js_str(slots.get("action"));
    let Value::Object(mut analysis) = json!({
        "status": "ready",
        "primary_intent": pick([text("primary_intent"), field(doc, "latest_intent"), "unknown".into()]),
        "summary": if summary.is_empty() { field(&fallback, "summary") } else { summary },
        "outcome": pick([text("outcome"), field(doc, "status"), "ended".into()]),
        "caller_goal": if caller_goal.is_empty() { field(&fallback, "caller_goal") } else { caller_goal },
        "key_facts": match parsed.get("key_facts") {
            Some(facts @ Value::Object(_)) => facts.clone(),
            _ => fallback.get("key_facts").cloned().unwrap_or_else(|| json!({})),
        },
        "recovered_slots": slots,
        "turn_intents": intents,
        "next_best_action": if roleplay || slot_action.is_empty() { "follow_up".to_string() } else { slot_action },
        "internal_handoff_brief": field(&fallback, "internal_handoff_brief"),
    }) else {
        unreachable!()
    };
    if let Some(card) = scorecard {
        analysis.insert("roleplay_scorecard".into(), Value::Object(card));
    }
    analysis.insert("generated_at".into(), json!(now_iso()));
    analysis.insert("error".into(), json!(""));
    analysis
}

/// Analyse a finished call. Never fails: any problem with the xAI call falls back to
/// the rule-based analysis with the reason recorded in `error`.
pub async fn generate_post_call_analysis(xai: &XaiRest, doc: &Doc) -> Doc {
    if !xai.has_key() {
        return heuristic_post_analysis(doc);
    }
    let company = {
        let c = field(doc, "client_company");
        if c.is_empty() {
            "the company".to_string()
        } else {
            c
        }
    };
    let messages = vec![
        json!({"role": "system", "content": analysis_prompt(is_roleplay_doc(doc), &company)}),
        json!({"role": "user", "content": build_conversation_text(doc)}),
    ];
    let result = async {
        let content = xai.chat_json(messages).await.map_err(|e| e.to_string())?;
        serde_json::from_str::<Value>(&content).map_err(|e| e.to_string())
    }
    .await;
    let fallback = heuristic_post_analysis(doc);
    match result {
        Ok(parsed) => build_from_model(doc, &parsed, fallback),
        Err(error) => {
            let mut fallback = fallback;
            fallback.insert("error".into(), json!(error));
            fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::XaiSettings;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    fn doc(value: Value) -> Doc {
        value.as_object().unwrap().clone()
    }

    fn enquiry() -> Doc {
        doc(json!({
            "conversation_id": "c1", "target_number": "9198", "status": "completed",
            "latest_intent": "provide_details",
            "slots": {"debt_type": "invoice", "amount": "$5,000", "action": "transfer"},
            "turns": [
                {"turn": 0, "transcript": "", "response": "Good morning", "intent": "opening_greeting"},
                {"turn": 1, "transcript": "I have an unpaid invoice", "response": "What amount?", "intent": "provide_details"},
                {"turn": 2, "transcript": "Five thousand", "response": "Thanks", "intent": "provide_details"},
                {"turn": 3, "transcript": "Can I talk to a person", "response": "Transferring", "intent": "request_transfer"},
                {"turn": 4, "transcript": "Thanks", "response": "", "intent": "realtime_utterance"}
            ]
        }))
    }

    fn roleplay(scenario: &str, staff: &[&str]) -> Doc {
        let turns: Vec<Value> = std::iter::once(
            json!({"turn": 0, "response": "Hello?", "intent": "roleplay_customer_opening"}),
        )
        .chain(
            staff
                .iter()
                .enumerate()
                .map(|(i, s)| json!({"turn": i + 1, "transcript": s, "response": "ok"})),
        )
        .collect();
        doc(
            json!({"mode": "roleplay", "target_number": "9199", "roleplay_scenario": scenario,
                   "roleplay_label": "Label", "slots": {}, "turns": turns}),
        )
    }

    #[test]
    fn conversation_text_for_enquiry() {
        let text = build_conversation_text(&enquiry());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Session: c1");
        assert_eq!(lines[1], "Line: 9198 · status=completed");
        assert!(lines[2].starts_with("Known slots: {"));
        assert_eq!(lines[3], "Transcript:");
        assert_eq!(lines[4], "[AI greeting] Good morning");
        assert_eq!(lines[5], "[Caller T1] I have an unpaid invoice");
        assert_eq!(lines[6], "[AI T1] (intent=provide_details) What amount?");
        assert_eq!(*lines.last().unwrap(), "[Caller T4] Thanks");
    }

    #[test]
    fn conversation_text_for_roleplay() {
        let text = build_conversation_text(&roleplay("fees", &["Good morning"]));
        assert!(text.contains("MODE: ROLEPLAY"));
        assert!(text.contains("Scenario: fees (Label)"));
        assert!(text.contains("[AI customer opening] Hello?"));
        assert!(text.contains("[Staff T1] Good morning"));
        assert!(text.contains("[AI customer T1] ok"));
    }

    #[test]
    fn heuristic_enquiry_summary() {
        let a = heuristic_post_analysis(&enquiry());
        assert_eq!(a["outcome"], "transferred");
        assert_eq!(a["primary_intent"], "provide_details");
        assert_eq!(
            a["summary"],
            "Caller discussed: I have an unpaid invoice · Five thousand · Can I talk to a person…. Debt type noted as invoice. Amount noted as $5,000. Outcome: transferred."
        );
        assert_eq!(a["caller_goal"], "I have an unpaid invoice");
        assert_eq!(
            a["turn_intents"],
            json!(["provide_details", "provide_details", "request_transfer"])
        );
        assert_eq!(
            a["key_facts"],
            json!({"debt_type": "invoice", "amount": "$5,000", "action": "transfer"})
        );
        assert_eq!(a["next_best_action"], "transfer");
        assert_eq!(a["internal_handoff_brief"], "");
        assert!(a.get("roleplay_scorecard").is_none());
    }

    #[test]
    fn heuristic_outcomes() {
        let case = |slots: Value, status: &str| {
            heuristic_post_analysis(&doc(json!({"slots": slots, "status": status, "turns": []})))
                ["outcome"]
                .clone()
        };
        assert_eq!(
            case(json!({"action": "callback"}), "active"),
            "callback_requested"
        );
        assert_eq!(
            case(json!({"action": "decline_debt_type"}), "active"),
            "declined_unsupported"
        );
        assert_eq!(case(json!({}), "completed"), "completed");
        assert_eq!(case(json!({}), "active"), "ended");
        let empty = heuristic_post_analysis(&doc(json!({"turns": []})));
        assert_eq!(empty["summary"], "Outcome: ended.");
        assert_eq!(empty["primary_intent"], "unknown");
        assert_eq!(empty["caller_goal"], "Not clearly captured");
    }

    #[test]
    fn scorecard_fees_scenario() {
        let card = heuristic_roleplay_scorecard(&roleplay(
            "fees",
            &[
                "Good morning, Slater Byrne",
                "There is no joining fee, commission only when we recover",
                "How much is the debt?",
            ],
        ));
        assert_eq!(card["greeting"], 8);
        assert_eq!(card["empathy"], 4);
        assert_eq!(card["discovery"], 9);
        assert_eq!(card["accuracy"], 9);
        assert_eq!(card["control"], 6);
        assert_eq!(card["overall_score"], 72);
        assert_eq!(card["grade"], "B");
        assert_eq!(
            card["strengths"],
            json!([
                "Professional opening",
                "Gathered key enquiry details",
                "SOP facts landed correctly"
            ])
        );
        assert_eq!(
            card["improvements"],
            json!(["Keep practicing structured intake"])
        );
    }

    #[test]
    fn scorecard_silent_angry_call() {
        let card = heuristic_roleplay_scorecard(&roleplay("angry", &[]));
        assert_eq!(
            (
                card["greeting"].as_i64(),
                card["empathy"].as_i64(),
                card["discovery"].as_i64(),
                card["accuracy"].as_i64(),
                card["control"].as_i64()
            ),
            (Some(2), Some(3), Some(3), Some(5), Some(4))
        );
        assert_eq!(card["overall_score"], 34);
        assert_eq!(card["grade"], "F");
        assert_eq!(card["strengths"], json!(["Completed practice call"]));
    }

    #[test]
    fn heuristic_roleplay_analysis() {
        let a = heuristic_post_analysis(&roleplay("transfer", &["Hello", "I'll transfer you"]));
        assert_eq!(a["primary_intent"], "roleplay_transfer");
        assert_eq!(a["outcome"], "practice_completed");
        assert_eq!(a["next_best_action"], "follow_up");
        assert_eq!(a["caller_goal"], "Practice scenario: Label");
        assert!(a["internal_handoff_brief"]
            .as_str()
            .unwrap()
            .starts_with("ROLEPLAY SCORECARD\nScenario: Label\nScore: "));
        assert_eq!(a["action_items"][0]["owner"], "staff");
        assert_eq!(a["key_facts"]["roleplay_scenario"], "transfer");
    }

    fn rest(base: &str, key: &str) -> XaiRest {
        XaiRest::new(XaiSettings {
            api_key: key.into(),
            base_url: base.into(),
            realtime_url: String::new(),
            grok_model: "m".into(),
            tts_voice: "eve".into(),
            language: "en".into(),
            stt_keyterm: None,
        })
    }

    #[tokio::test]
    async fn model_analysis_overlays_fallback() {
        let server = MockServer::start().await;
        let reply = json!({"primary_intent": "new_enquiry", "summary": " Caller has an invoice. ",
                           "outcome": "transferred", "recovered_slots": {"reference": "INV-1", "amount": null}});
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"choices": [{"message": {"content": reply.to_string()}}]}),
                ),
            )
            .mount(&server)
            .await;
        let a = generate_post_call_analysis(&rest(&server.uri(), "k"), &enquiry()).await;
        assert_eq!(a["primary_intent"], "new_enquiry");
        assert_eq!(a["summary"], "Caller has an invoice.");
        assert_eq!(
            a["caller_goal"], "I have an unpaid invoice",
            "missing fields come from the fallback"
        );
        assert_eq!(a["recovered_slots"]["reference"], "INV-1");
        assert_eq!(a["recovered_slots"]["amount"], "$5,000");
        assert_eq!(a["next_best_action"], "transfer");
        assert_eq!(a["error"], "");
        assert!(a.get("risks").is_none());
    }

    #[tokio::test]
    async fn failures_fall_back_with_error_recorded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let a = generate_post_call_analysis(&rest(&server.uri(), "k"), &enquiry()).await;
        assert_eq!(a["outcome"], "transferred");
        assert!(a["error"].as_str().unwrap().contains("500"));
        let offline = generate_post_call_analysis(&rest(&server.uri(), ""), &enquiry()).await;
        assert_eq!(offline["error"], "");
    }

    #[tokio::test]
    async fn roleplay_scorecard_merges_model_values() {
        let server = MockServer::start().await;
        let reply = json!({"roleplay_scorecard": {"overall_score": 91, "grade": "A", "coach_notes": "Great"}});
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"choices": [{"message": {"content": reply.to_string()}}]}),
                ),
            )
            .mount(&server)
            .await;
        let a =
            generate_post_call_analysis(&rest(&server.uri(), "k"), &roleplay("fees", &["Hello"]))
                .await;
        let card = &a["roleplay_scorecard"];
        assert_eq!(card["overall_score"], 91);
        assert_eq!(card["scenario"], "fees");
        assert_eq!(card["scenario_label"], "Label");
        assert_eq!(
            card["greeting"], 8,
            "dimensions the model left out keep the heuristic value"
        );
        assert_eq!(a["next_best_action"], "follow_up");
    }
}
