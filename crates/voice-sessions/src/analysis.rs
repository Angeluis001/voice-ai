//! Post-call analysis: the text sent to the model, the offline (heuristic) analysis,
//! merging a model answer, roleplay scorecards and the generated content pack.
//! Everything here is a pure function of a session.
use crate::models::{
    utc_now_iso, CallActionItem, CallContentArtifact, CallContentPack, CallPostAnalysis,
    CallStakeholder, LiveCallSession, RoleplayScorecard,
};
use crate::prompts::{COLLECTIONS_ANALYSIS_PROMPT, POST_ANALYSIS_PROMPT, ROLEPLAY_ANALYSIS_PROMPT};
use crate::pyjson::{get, py_json_dumps, py_str, py_title, safe_int, str_or, truthy, Loose, Skip};
use serde_json::{json, Map, Value};

/// Roleplay scenarios where the staff member plays the collector.
pub const COLLECTIONS_SCENARIOS: [&str; 8] = [
    "pay",
    "promise",
    "gate",
    "vm",
    "hardship",
    "restructure",
    "later",
    "dead",
];

const OPENING_INTENTS: [&str; 2] = ["opening_greeting", "roleplay_customer_opening"];

/// A training call where the AI plays the other party (mode/profile `roleplay`, or extension 9199).
pub fn is_roleplay_session(session: &LiveCallSession) -> bool {
    session.mode.to_lowercase() == "roleplay"
        || session
            .prompt_profile
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            == "roleplay"
        || session.target_number.as_deref() == Some("9199")
}

/// A roleplay scenario from the outbound-collections set.
pub fn is_collections_session(session: &LiveCallSession) -> bool {
    let key = session.roleplay_scenario.trim().to_lowercase();
    COLLECTIONS_SCENARIOS.contains(&key.as_str())
}

/// The system prompt used to analyse this session.
pub fn analysis_system_prompt(session: &LiveCallSession) -> &'static str {
    let roleplay = is_roleplay_session(session);
    if roleplay && is_collections_session(session) {
        COLLECTIONS_ANALYSIS_PROMPT
    } else if roleplay {
        ROLEPLAY_ANALYSIS_PROMPT
    } else {
        POST_ANALYSIS_PROMPT
    }
}

fn or_str<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() {
        default
    } else {
        value
    }
}

fn turn_label(turn: Option<i64>) -> String {
    turn.map(|n| n.to_string()).unwrap_or_else(|| "None".into())
}

/// The conversation as plain text, labelled by speaker, for the analysis model.
pub fn conversation_for_analysis(session: &LiveCallSession) -> String {
    let roleplay = is_roleplay_session(session);
    let collections = roleplay && is_collections_session(session);
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Session: {}", session.session_id));
    lines.push(format!(
        "Line: {} · status={}",
        or_str(session.target_number.as_deref().unwrap_or(""), "—"),
        session.status
    ));
    if collections {
        lines.push(
            "MODE: ROLEPLAY — Staff = SBR collector; AI = debtor / gatekeeper / voicemail".into(),
        );
        lines.push(format!(
            "Scenario: {} ({})",
            or_str(&session.roleplay_scenario, "pay"),
            session.roleplay_label
        ));
    } else if roleplay {
        lines.push("MODE: ROLEPLAY — Staff = receptionist trainee; AI = customer persona".into());
        lines.push(format!(
            "Scenario: {} ({})",
            or_str(&session.roleplay_scenario, "invoice"),
            session.roleplay_label
        ));
    }
    lines.push(format!(
        "Known slots: {}",
        py_json_dumps(&Value::Object(session.slots.clone()))
    ));
    lines.push("Transcript:".into());
    for turn in &session.turns {
        let opening = OPENING_INTENTS.contains(&turn.intent.as_str())
            || (turn.turn == Some(0) && !turn.response.is_empty() && turn.transcript.is_empty());
        if opening {
            let label = if collections {
                "[AI party opening]"
            } else if roleplay {
                "[AI customer opening]"
            } else {
                "[AI greeting]"
            };
            lines.push(format!("{label} {}", turn.response));
            continue;
        }
        let n = turn_label(turn.turn);
        if !turn.transcript.is_empty() {
            let who = if roleplay { "Staff" } else { "Caller" };
            lines.push(format!("[{who} T{n}] {}", turn.transcript));
        }
        if !turn.response.is_empty() {
            let intent_bit = if turn.intent.is_empty() {
                String::new()
            } else {
                format!(" (intent={})", turn.intent)
            };
            let who = if collections {
                "AI party"
            } else if roleplay {
                "AI customer"
            } else {
                "AI"
            };
            lines.push(format!("[{who} T{n}]{intent_bit} {}", turn.response));
        }
    }
    if !session
        .turns
        .iter()
        .any(|t| !t.transcript.is_empty() || !t.response.is_empty())
    {
        if !session.latest_transcript.is_empty() {
            let who = if roleplay {
                "[Staff latest]"
            } else {
                "[Caller latest]"
            };
            lines.push(format!("{who} {}", session.latest_transcript));
        }
        if !session.latest_response.is_empty() {
            let who = if collections {
                "[AI party latest]"
            } else if roleplay {
                "[AI customer latest]"
            } else {
                "[AI latest]"
            };
            lines.push(format!("{who} {}", session.latest_response));
        }
    }
    lines.join("\n")
}

fn grade_for(score: i64) -> &'static str {
    match score {
        s if s >= 85 => "A",
        s if s >= 70 => "B",
        s if s >= 55 => "C",
        s if s >= 40 => "D",
        _ => "F",
    }
}

fn card_int(raw: &Map<String, Value>, key: &str) -> Loose<i64> {
    match raw.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(n)) => match n.as_i64() {
            Some(i) => Ok(i),
            None => n
                .as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() < 9.0e18)
                .map(|f| f as i64)
                .ok_or(Skip),
        },
        Some(Value::String(s)) => s.trim().parse().map_err(|_| Skip),
        Some(Value::Bool(b)) => Ok(i64::from(*b)),
        Some(_) => Err(Skip),
    }
}

fn card_str(raw: &Map<String, Value>, key: &str) -> Loose<String> {
    match raw.get(key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(Skip),
    }
}

fn card_list(raw: &Map<String, Value>, key: &str) -> Loose<Vec<String>> {
    match raw.get(key) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string).ok_or(Skip))
            .collect(),
        Some(_) => Err(Skip),
    }
}

/// Read a scorecard returned by the model: fills the scenario when missing, clamps
/// scores (overall 0-100, dimensions 0-10) and derives the grade when absent.
/// `None` when `raw` is not a usable scorecard object.
pub fn parse_roleplay_scorecard(
    raw: &Value,
    scenario: &str,
    scenario_label: &str,
) -> Option<RoleplayScorecard> {
    let raw = raw.as_object().filter(|m| !m.is_empty())?;
    let build = || -> Loose<RoleplayScorecard> {
        Ok(RoleplayScorecard {
            scenario: card_str(raw, "scenario")?,
            scenario_label: card_str(raw, "scenario_label")?,
            overall_score: card_int(raw, "overall_score")?,
            grade: card_str(raw, "grade")?,
            greeting: card_int(raw, "greeting")?,
            empathy: card_int(raw, "empathy")?,
            discovery: card_int(raw, "discovery")?,
            accuracy: card_int(raw, "accuracy")?,
            control: card_int(raw, "control")?,
            strengths: card_list(raw, "strengths")?,
            improvements: card_list(raw, "improvements")?,
            coach_notes: card_str(raw, "coach_notes")?,
        })
    };
    let mut card = build().ok()?;
    if card.scenario.is_empty() {
        card.scenario = scenario.to_string();
    }
    if card.scenario_label.is_empty() {
        card.scenario_label = scenario_label.to_string();
    }
    card.overall_score = card.overall_score.clamp(0, 100);
    for score in [
        &mut card.greeting,
        &mut card.empathy,
        &mut card.discovery,
        &mut card.accuracy,
        &mut card.control,
    ] {
        *score = (*score).clamp(0, 10);
    }
    if card.grade.is_empty() {
        card.grade = grade_for(card.overall_score).to_string();
    }
    Some(card)
}

fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

fn spoken_bits(session: &LiveCallSession) -> Vec<&str> {
    session
        .turns
        .iter()
        .map(|t| t.transcript.trim())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Offline coaching score from transcript keywords, used when the model is unavailable.
pub fn heuristic_roleplay_scorecard(session: &LiveCallSession) -> RoleplayScorecard {
    let scenario = or_str(&session.roleplay_scenario, "invoice").to_string();
    let label = if session.roleplay_label.is_empty() {
        py_title(&scenario.replace('_', " "))
    } else {
        session.roleplay_label.clone()
    };
    let staff_bits: Vec<String> = spoken_bits(session)
        .iter()
        .map(|t| t.to_lowercase())
        .collect();
    let joined = staff_bits.join(" ");
    let joined = joined.as_str();
    let slots = &session.slots;

    let mut greeting = if staff_bits.is_empty() { 2 } else { 6 };
    if has_any(
        joined,
        &[
            "good morning",
            "hello",
            "hi ",
            "slater",
            "how can i",
            "speaking",
        ],
    ) {
        greeting = 8;
    }

    let mut empathy = 4;
    if has_any(
        joined,
        &["sorry", "understand", "frustrat", "hear you", "appreciate"],
    ) {
        empathy = 8;
    } else if scenario == "angry" {
        empathy = 3;
    }

    let mut discovery = 3;
    if truthy(get(slots, "debt_type"))
        || has_any(joined, &["debt", "invoice", "type of", "what kind"])
    {
        discovery += 3;
    }
    if truthy(get(slots, "amount")) || has_any(joined, &["amount", "how much", "dollar", "$"]) {
        discovery += 3;
    }
    let discovery = discovery.min(10);

    let accuracy = match scenario.as_str() {
        "fees" => {
            if has_any(
                joined,
                &[
                    "no joining",
                    "no fee",
                    "commission",
                    "only when",
                    "successfully",
                ],
            ) {
                9
            } else if has_any(joined, &["fee", "cost", "charge"]) {
                6
            } else {
                3
            }
        }
        "unsupported" => {
            let declined = [
                "cannot",
                "can't",
                "unable",
                "not able",
                "don't take",
                "letter of demand",
                "template",
            ];
            if has_any(joined, &declined) {
                9
            } else {
                3
            }
        }
        "transfer" => {
            if has_any(
                joined,
                &[
                    "transfer",
                    "put you through",
                    "connect you",
                    "colleague",
                    "person",
                ],
            ) {
                8
            } else {
                4
            }
        }
        _ => 5,
    };

    let mut control: i64 = if staff_bits.len() >= 2 { 6 } else { 4 };
    if joined.chars().count() > 800 {
        control = (control - 2).max(3);
    }

    let dims = [greeting, empathy, discovery, accuracy, control];
    let overall = (dims.iter().sum::<i64>() as f64 / dims.len() as f64 * 10.0).round() as i64;

    let mut strengths: Vec<String> = Vec::new();
    let mut improvements: Vec<String> = Vec::new();
    if greeting >= 7 {
        strengths.push("Professional opening".into());
    } else {
        improvements.push("Open with a clear company greeting".into());
    }
    if discovery >= 7 {
        strengths.push("Gathered key enquiry details".into());
    } else {
        improvements.push("Ask debt type and amount early".into());
    }
    if accuracy >= 7 {
        strengths.push("SOP facts landed correctly".into());
    } else {
        improvements.push("Review SOP for fees / unsupported / transfer language".into());
    }
    if empathy >= 7 {
        strengths.push("Showed empathy".into());
    } else if scenario == "angry" || scenario == "transfer" {
        improvements.push("Acknowledge frustration before process questions".into());
    }
    if strengths.is_empty() {
        strengths.push("Completed practice call".into());
    }
    if improvements.is_empty() {
        improvements.push("Keep practicing structured intake".into());
    }

    let coach_notes = format!(
        "Practice scenario: {label}. Offline score from transcript heuristics. \
         Staff turns heard: {}. Re-run Summary Inform with force if xAI was down.",
        staff_bits.len()
    );
    RoleplayScorecard {
        scenario,
        scenario_label: label,
        overall_score: overall,
        grade: grade_for(overall).to_string(),
        greeting,
        empathy,
        discovery,
        accuracy,
        control,
        strengths,
        improvements,
        coach_notes,
    }
}

/// Action items from a model answer. Entries without a task are dropped.
pub fn parse_action_items(raw: &Value) -> Vec<CallActionItem> {
    let Some(entries) = raw.as_array() else {
        return vec![];
    };
    entries
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|entry| {
            let task = str_or(get(entry, "task"), "").trim().to_string();
            if task.is_empty() {
                return None;
            }
            let due = get(entry, "due");
            Some(CallActionItem {
                owner: str_or(get(entry, "owner"), "staff"),
                task,
                due: if due.is_null() || due.as_str() == Some("") {
                    None
                } else {
                    Some(py_str(due))
                },
            })
        })
        .collect()
}

/// Stakeholders from a model answer.
pub fn parse_stakeholders(raw: &Value) -> Vec<CallStakeholder> {
    let Some(entries) = raw.as_array() else {
        return vec![];
    };
    entries
        .iter()
        .filter_map(Value::as_object)
        .map(|entry| {
            let name = get(entry, "name");
            CallStakeholder {
                role: str_or(get(entry, "role"), "caller"),
                name: if name.is_null() || name.as_str() == Some("") {
                    None
                } else {
                    Some(py_str(name))
                },
                notes: str_or(get(entry, "notes"), ""),
            }
        })
        .collect()
}

fn scorecard_brief(card: &RoleplayScorecard) -> String {
    format!(
        "ROLEPLAY SCORECARD\nScenario: {}\nScore: {}/100 ({})\nStrengths: {}\nImprovements: {}\n{}",
        card.scenario_label,
        card.overall_score,
        card.grade,
        or_str(&card.strengths.join(", "), "—"),
        or_str(&card.improvements.join(", "), "—"),
        card.coach_notes
    )
}

fn first_three(bits: &[&str]) -> String {
    let head: Vec<&str> = bits.iter().take(3).copied().collect();
    format!(
        "{}{}",
        head.join(" · "),
        if bits.len() > 3 { "…" } else { "" }
    )
}

/// Offline analysis built from slots, intents and transcripts, used when the model
/// is unavailable and to fill gaps in a model answer.
pub fn heuristic_post_analysis(session: &LiveCallSession) -> CallPostAnalysis {
    let roleplay = is_roleplay_session(session);
    let slots = session.slots.clone();
    let action = match session.latest_action.as_deref() {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => str_or(get(&slots, "action"), "continue"),
    };
    let intents: Vec<String> = session
        .turns
        .iter()
        .map(|t| t.intent.as_str())
        .filter(|i| !i.is_empty() && !OPENING_INTENTS.contains(i) && *i != "assistant_prompt")
        .map(str::to_string)
        .collect();
    let mut primary = if session.latest_intent.is_empty() {
        intents.last().cloned().unwrap_or_else(|| "unknown".into())
    } else {
        session.latest_intent.clone()
    };
    let outcome: String = if roleplay {
        primary = format!("roleplay_{}", or_str(&session.roleplay_scenario, "invoice"));
        if session.turn_count > 1 {
            "practice_completed"
        } else {
            "practice_partial"
        }
        .into()
    } else if action == "transfer" {
        if ["", "unknown", "realtime_utterance"].contains(&primary.as_str()) {
            primary = "request_transfer".into();
        }
        "transferred".into()
    } else if action == "callback" {
        "callback_requested".into()
    } else if action == "decline_debt_type" {
        "declined_unsupported".into()
    } else if action == "complete" || session.status == "completed" {
        "completed".into()
    } else {
        or_str(&session.status, "ended").to_string()
    };

    let caller_bits = spoken_bits(session);
    let debt_value = get(&slots, "debt_type");
    let amount_value = get(&slots, "amount");
    let outcome_words = outcome.replace('_', " ");
    let mut summary_parts: Vec<String> = Vec::new();
    if roleplay {
        let label = or_str(
            &session.roleplay_label,
            or_str(&session.roleplay_scenario, "practice"),
        );
        summary_parts.push(format!("Roleplay training ({label})."));
        if !caller_bits.is_empty() {
            summary_parts.push(format!("Staff said: {}.", first_three(&caller_bits)));
        }
        summary_parts.push(format!("Outcome: {outcome_words}."));
    } else {
        if !caller_bits.is_empty() {
            summary_parts.push(format!("Caller discussed: {}.", first_three(&caller_bits)));
        }
        if truthy(debt_value) {
            summary_parts.push(format!("Debt type noted as {}.", py_str(debt_value)));
        }
        if truthy(amount_value) {
            summary_parts.push(format!("Amount noted as {}.", py_str(amount_value)));
        }
        summary_parts.push(format!("Outcome: {outcome_words}."));
    }

    let mut key_facts = Map::new();
    for key in ["debt_type", "amount", "staff_name", "reference"] {
        let value = get(&slots, key);
        if !(value.is_null() || matches!(value.as_str(), Some("") | Some("null"))) {
            key_facts.insert(key.to_string(), value.clone());
        }
    }
    if !(action.is_empty() || action == "null") {
        key_facts.insert("action".into(), json!(action));
    }
    if roleplay && !session.roleplay_scenario.is_empty() {
        key_facts.insert("roleplay_scenario".into(), json!(session.roleplay_scenario));
    }

    let mut risks: Vec<String> = Vec::new();
    if action == "decline_debt_type" || primary.contains("unsupported") {
        risks.push("unsupported_debt".into());
    }
    if !truthy(amount_value) && !roleplay {
        risks.push("no_amount".into());
    }
    if action == "transfer" {
        risks.push("transfer_requested".into());
    }
    if caller_bits.is_empty() {
        risks.push("incomplete_details".into());
    }

    let item = |owner: &str, task: &str, due: Option<&str>| CallActionItem {
        owner: owner.into(),
        task: task.into(),
        due: due.map(str::to_string),
    };
    let action_items = vec![if roleplay {
        item(
            "staff",
            "Review roleplay scorecard and retry weak scenarios",
            Some("same_day"),
        )
    } else if action == "transfer" {
        item(
            "staff",
            "Take over call / call back customer after transfer",
            Some("same_day"),
        )
    } else if action == "callback" {
        item("staff", "Schedule callback with caller", Some("same_day"))
    } else if action == "decline_debt_type" {
        item(
            "caller",
            "Use free Letter of Demand template on website",
            None,
        )
    } else {
        item(
            "staff",
            "Review enquiry and follow up if needed",
            Some("same_day"),
        )
    }];

    let debt = str_or(debt_value, "not stated");
    let amount = str_or(amount_value, "not stated");
    let scorecard = roleplay.then(|| heuristic_roleplay_scorecard(session));
    let first_bit = caller_bits.first().copied();
    let (handoff, email, caller_goal) = match &scorecard {
        Some(card) => (
            scorecard_brief(card),
            String::new(),
            format!("Practice scenario: {}", card.scenario_label),
        ),
        None => {
            let handoff = format!(
                "Caller goal: {}\nDebt type: {debt}\nAmount: {amount}\nRecommended action: {action}\nOutcome so far: {outcome}",
                first_bit.unwrap_or("unclear")
            );
            let amount_bit = if amount != "not stated" {
                format!(", amount {amount}")
            } else {
                String::new()
            };
            let email = format!(
                "Subject: Following up on your enquiry\n\nHello,\n\n\
                 Thank you for calling Slater Byrne Recoveries. Regarding your enquiry ({debt}{amount_bit}): {}\n\n\
                 Please reply if you need anything further.\n\nKind regards,\nEnquiries team",
                summary_parts.join(" ")
            );
            (
                handoff,
                email,
                first_bit.unwrap_or("Not clearly captured").to_string(),
            )
        }
    };

    let next_best_action = if roleplay {
        "follow_up".to_string()
    } else if [
        "continue",
        "transfer",
        "callback",
        "complete",
        "decline_debt_type",
    ]
    .contains(&action.as_str())
    {
        action.clone()
    } else {
        "follow_up".to_string()
    };

    CallPostAnalysis {
        status: "ready".into(),
        primary_intent: or_str(&primary, "unknown").to_string(),
        summary: or_str(
            &summary_parts.join(" "),
            "Call ended with limited transcript captured.",
        )
        .to_string(),
        outcome,
        caller_goal,
        key_facts,
        recovered_slots: slots,
        turn_intents: intents,
        risks,
        action_items,
        next_best_action,
        stakeholders: vec![CallStakeholder {
            role: if roleplay { "trainee" } else { "caller" }.into(),
            name: None,
            notes: first_bit.unwrap_or("").to_string(),
        }],
        follow_up_email_draft: email,
        internal_handoff_brief: handoff,
        product_feedback: vec![],
        roleplay_scorecard: scorecard,
        generated_at: utc_now_iso(),
        error: String::new(),
    }
}

fn truthy_strings(raw: &Value) -> Vec<String> {
    raw.as_array()
        .map(|items| items.iter().filter(|v| truthy(v)).map(py_str).collect())
        .unwrap_or_default()
}

/// Build the analysis from the model's JSON answer, filling any gaps from the
/// heuristic analysis so the summary view is never empty.
/// `Err` (with a reason) when the answer is not a JSON object.
pub fn post_analysis_from_model(
    session: &LiveCallSession,
    content: &str,
) -> Result<CallPostAnalysis, String> {
    let parsed: Value =
        serde_json::from_str(content).map_err(|e| format!("Model returned invalid JSON: {e}"))?;
    let parsed = parsed
        .as_object()
        .ok_or_else(|| "Model returned JSON that is not an object".to_string())?;
    let roleplay = is_roleplay_session(session);

    let mut merged_slots = session.slots.clone();
    if let Some(recovered) = get(parsed, "recovered_slots").as_object() {
        for (key, value) in recovered {
            if !(value.is_null() || value.as_str() == Some("")) {
                merged_slots.insert(key.clone(), value.clone());
            }
        }
    }
    let key_facts = get(parsed, "key_facts")
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut risks = truthy_strings(get(parsed, "risks"));
    let product_feedback = truthy_strings(get(parsed, "product_feedback"));
    let mut action_items = parse_action_items(get(parsed, "action_items"));
    let mut email = str_or(get(parsed, "follow_up_email_draft"), "")
        .trim()
        .to_string();
    let mut handoff = str_or(get(parsed, "internal_handoff_brief"), "")
        .trim()
        .to_string();

    let fallback = heuristic_post_analysis(session);
    if action_items.is_empty() {
        action_items = fallback.action_items;
    }
    if risks.is_empty() {
        risks = fallback.risks;
    }
    if email.is_empty() && !roleplay {
        email = fallback.follow_up_email_draft;
    }
    if handoff.is_empty() {
        handoff = fallback.internal_handoff_brief;
    }
    let mut stakeholders = parse_stakeholders(get(parsed, "stakeholders"));
    if stakeholders.is_empty() {
        stakeholders = fallback.stakeholders;
    }
    let mut scorecard = None;
    if roleplay {
        scorecard = parse_roleplay_scorecard(
            get(parsed, "roleplay_scorecard"),
            or_str(&session.roleplay_scenario, "invoice"),
            &session.roleplay_label,
        )
        .or(fallback.roleplay_scorecard);
        if let (Some(card), true) = (&scorecard, handoff.is_empty()) {
            handoff = scorecard_brief(card);
        }
    }

    let turn_intents = session
        .turns
        .iter()
        .map(|t| t.intent.as_str())
        .filter(|i| !i.is_empty() && !OPENING_INTENTS.contains(i))
        .map(str::to_string)
        .collect();
    let primary_intent = match get(parsed, "primary_intent") {
        v if truthy(v) => py_str(v),
        _ => or_str(&session.latest_intent, "unknown").to_string(),
    };
    let summary = str_or(get(parsed, "summary"), "").trim().to_string();
    let outcome = match get(parsed, "outcome") {
        v if truthy(v) => py_str(v),
        _ => session.status.clone(),
    };
    let caller_goal = str_or(get(parsed, "caller_goal"), "").trim().to_string();
    let next_best_action = match get(parsed, "next_best_action") {
        v if truthy(v) => py_str(v),
        _ => str_or(get(&merged_slots, "action"), "follow_up"),
    };

    Ok(CallPostAnalysis {
        status: "ready".into(),
        primary_intent,
        summary: if summary.is_empty() {
            fallback.summary
        } else {
            summary
        },
        outcome: if outcome.is_empty() {
            fallback.outcome
        } else {
            outcome
        },
        caller_goal: if caller_goal.is_empty() {
            fallback.caller_goal
        } else {
            caller_goal
        },
        key_facts: if key_facts.is_empty() {
            fallback.key_facts
        } else {
            key_facts
        },
        recovered_slots: merged_slots,
        turn_intents,
        risks,
        action_items,
        next_best_action,
        stakeholders,
        follow_up_email_draft: email,
        internal_handoff_brief: handoff,
        product_feedback,
        roleplay_scorecard: scorecard,
        generated_at: utc_now_iso(),
        error: String::new(),
    })
}

/// Ready-to-use content for a call: follow-up email, handoff brief, SMS, fee FAQ,
/// an unsupported-debt checklist (only when relevant) and a JSON enquiry log.
/// Uses `analysis`, else the session's stored analysis, else the heuristic one.
pub fn generate_call_content(
    session: &LiveCallSession,
    analysis: Option<&CallPostAnalysis>,
) -> CallContentPack {
    let computed;
    let analysis = match analysis.or(session.post_analysis.as_ref()) {
        Some(a) => a,
        None => {
            computed = heuristic_post_analysis(session);
            &computed
        }
    };
    let mut slots = session.slots.clone();
    for (key, value) in &analysis.recovered_slots {
        slots.insert(key.clone(), value.clone());
    }
    let debt = str_or(get(&slots, "debt_type"), "not stated");
    let amount = str_or(get(&slots, "amount"), "not stated");
    let action = if !analysis.next_best_action.is_empty() {
        analysis.next_best_action.clone()
    } else {
        match session.latest_action.as_deref() {
            Some(a) if !a.is_empty() => a.to_string(),
            _ => str_or(get(&slots, "action"), "continue"),
        }
    };
    let summary = or_str(
        &analysis.summary,
        "Your recent enquiry with Slater Byrne Recoveries.",
    );
    let goal = or_str(&analysis.caller_goal, "your enquiry");
    let artifact = |key: &str, title: &str, body: String, content_type: &str| CallContentArtifact {
        key: key.into(),
        title: title.into(),
        body,
        content_type: content_type.into(),
    };
    let mut artifacts = Vec::new();

    let mut email = analysis.follow_up_email_draft.trim().to_string();
    if email.is_empty() {
        email = format!(
            "Subject: Following up on your enquiry\n\nHello,\n\n\
             Thank you for calling Slater Byrne Recoveries regarding {goal}.\n\n\
             {summary}\n\n\
             Debt type: {debt}\nAmount: {amount}\n\n\
             Please reply if you need anything further.\n\n\
             Kind regards,\nEnquiries team"
        );
    }
    artifacts.push(artifact(
        "follow_up_email",
        "Follow-up email",
        email,
        "email",
    ));

    let mut handoff = or_str(&session.handoff_brief, &analysis.internal_handoff_brief)
        .trim()
        .to_string();
    if handoff.is_empty() {
        handoff = format!(
            "INTERNAL HANDOFF BRIEF\nCall: {}\nIntent: {}\nAction: {action}\nDebt type: {debt}\nAmount: {amount}\n\
             Caller last said: {}\nAI last said: {}\nPlease greet the caller and continue intake.",
            session.session_id,
            or_str(&analysis.primary_intent, or_str(&session.latest_intent, "—")),
            or_str(&session.latest_transcript, "—"),
            or_str(&session.latest_response, "—"),
        );
    }
    artifacts.push(artifact(
        "handoff_brief",
        "Internal handoff brief",
        handoff,
        "text",
    ));

    let amount_bit = if amount != "not stated" {
        format!(" ({amount})")
    } else {
        String::new()
    };
    let sms = format!(
        "Hi, thanks for calling Slater Byrne Recoveries about {debt}{amount_bit}. \
         We'll follow up as discussed. Reply STOP to opt out."
    );
    artifacts.push(artifact("follow_up_sms", "Follow-up SMS", sms, "sms"));

    let fee_blurb = "Fees at Slater Byrne Recoveries:\n\
                     - No joining fee\n\
                     - No account maintenance fee\n\
                     - Commission only if we successfully recover the debt\n\
                     Suitable for a short verbal answer or email insert.";
    artifacts.push(artifact(
        "fee_faq",
        "Fee FAQ blurb",
        fee_blurb.to_string(),
        "text",
    ));

    if action == "decline_debt_type"
        || analysis
            .primary_intent
            .to_lowercase()
            .contains("unsupported")
        || analysis
            .risks
            .iter()
            .any(|r| r.to_lowercase().contains("unsupported"))
    {
        let lod = format!(
            "UNSUPPORTED DEBT — Letter of Demand checklist\n\
             Debt type noted: {debt}\n\
             1. Explain we cannot take this debt type as a new file.\n\
             2. Direct caller to slaterbyrne.com.au → Services → free Letter of Demand template.\n\
             3. Confirm they can download and use the template themselves.\n\
             4. Do not open a recovery file for residential/rental/childcare/vet/gym membership.\n\
             5. Offer transfer only if they insist on speaking to a person about other matters."
        );
        artifacts.push(artifact(
            "unsupported_lod_checklist",
            "Unsupported debt / LOD checklist",
            lod,
            "checklist",
        ));
    }

    let enquiry_log = json!({
        "session_id": session.session_id,
        "status": session.status,
        "primary_intent": analysis.primary_intent,
        "outcome": analysis.outcome,
        "slots": slots,
        "summary": analysis.summary,
        "next_best_action": action,
        "risks": analysis.risks,
    });
    let log_body = serde_json::to_string_pretty(&enquiry_log).unwrap_or_else(|_| "{}".into());
    artifacts.push(artifact(
        "enquiry_log_json",
        "Enquiry log (JSON)",
        log_body,
        "text",
    ));

    CallContentPack {
        status: "ready".into(),
        artifacts,
        generated_at: utc_now_iso(),
    }
}

/// `int()`-style read of a latency value, exposed for the session loader.
pub(crate) fn latency_ms(timings: Option<&Map<String, Value>>) -> Option<i64> {
    let timings = timings.filter(|t| !t.is_empty())?;
    safe_int(get(timings, "total"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::VoiceInResult;

    fn turn(n: i64, transcript: &str, intent: &str, response: &str) -> VoiceInResult {
        let mut t = VoiceInResult::new("telephony", &format!("r{n}"), transcript, intent, response);
        t.turn = Some(n);
        t.phase = "completed".into();
        t
    }

    fn session(turns: Vec<VoiceInResult>) -> LiveCallSession {
        let mut s = LiveCallSession::new("s1");
        s.status = "completed".into();
        s.turn_count = turns.len() as i64;
        s.turns = turns;
        s
    }

    fn slots(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn roleplay_and_collections_detection() {
        let mut s = session(vec![]);
        assert!(!is_roleplay_session(&s));
        s.mode = "Roleplay".into();
        assert!(is_roleplay_session(&s));
        s.mode = "realtime".into();
        s.prompt_profile = Some("ROLEPLAY".into());
        assert!(is_roleplay_session(&s));
        s.prompt_profile = None;
        s.target_number = Some("9199".into());
        assert!(is_roleplay_session(&s));
        assert!(!is_collections_session(&s));
        s.roleplay_scenario = " Hardship ".into();
        assert!(is_collections_session(&s));
        assert_eq!(analysis_system_prompt(&s), COLLECTIONS_ANALYSIS_PROMPT);
        s.roleplay_scenario = "fees".into();
        assert_eq!(analysis_system_prompt(&s), ROLEPLAY_ANALYSIS_PROMPT);
        s.target_number = Some("1000".into());
        assert_eq!(analysis_system_prompt(&s), POST_ANALYSIS_PROMPT);
    }

    #[test]
    fn conversation_text_for_a_normal_call() {
        let mut s = session(vec![
            turn(0, "", "opening_greeting", "Welcome."),
            turn(
                1,
                "I have an unpaid invoice",
                "new_enquiry",
                "What is the amount?",
            ),
            turn(2, "Five thousand", "", ""),
        ]);
        s.target_number = Some("1000".into());
        s.slots = slots(json!({"debt_type": "invoice", "amount": 5000}));
        assert_eq!(
            conversation_for_analysis(&s),
            "Session: s1\nLine: 1000 · status=completed\n\
             Known slots: {\"debt_type\": \"invoice\", \"amount\": 5000}\nTranscript:\n\
             [AI greeting] Welcome.\n\
             [Caller T1] I have an unpaid invoice\n\
             [AI T1] (intent=new_enquiry) What is the amount?\n\
             [Caller T2] Five thousand"
        );
    }

    #[test]
    fn conversation_text_for_roleplay_and_empty_turns() {
        let mut s = session(vec![
            turn(0, "", "", "My invoice is late!"),
            turn(1, "Hello, Slater Byrne", "x", "Hi"),
        ]);
        s.mode = "roleplay".into();
        s.roleplay_label = "Angry".into();
        let text = conversation_for_analysis(&s);
        assert!(text.contains("Line: — · status=completed"));
        assert!(text.contains("MODE: ROLEPLAY — Staff = receptionist trainee; AI = customer persona\nScenario: invoice (Angry)"));
        assert!(text.contains("[AI customer opening] My invoice is late!"));
        assert!(text.contains("[Staff T1] Hello, Slater Byrne\n[AI customer T1] (intent=x) Hi"));

        s.roleplay_scenario = "vm".into();
        s.turns.clear();
        s.latest_transcript = "leaving a message".into();
        s.latest_response = "beep".into();
        let text = conversation_for_analysis(&s);
        assert!(text.contains("Staff = SBR collector"));
        assert!(
            text.ends_with("Transcript:\n[Staff latest] leaving a message\n[AI party latest] beep")
        );
    }

    #[test]
    fn scorecard_is_clamped_and_graded() {
        let raw = json!({"overall_score": 140, "greeting": 12, "empathy": -3, "discovery": "7", "accuracy": 9.0,
                         "strengths": ["calm"], "coach_notes": "ok"});
        let card = parse_roleplay_scorecard(&raw, "fees", "Fees question").unwrap();
        assert_eq!(
            (
                card.overall_score,
                card.greeting,
                card.empathy,
                card.discovery,
                card.accuracy
            ),
            (100, 10, 0, 7, 9)
        );
        assert_eq!(card.grade, "A");
        assert_eq!(
            (card.scenario.as_str(), card.scenario_label.as_str()),
            ("fees", "Fees question")
        );

        for (score, grade) in [
            (85, "A"),
            (84, "B"),
            (70, "B"),
            (69, "C"),
            (55, "C"),
            (54, "D"),
            (40, "D"),
            (39, "F"),
        ] {
            let card = parse_roleplay_scorecard(&json!({"overall_score": score}), "", "").unwrap();
            assert_eq!(card.grade, grade, "score {score}");
        }
        let kept = parse_roleplay_scorecard(
            &json!({"overall_score": 10, "grade": "B", "scenario": "x"}),
            "y",
            "",
        )
        .unwrap();
        assert_eq!((kept.grade.as_str(), kept.scenario.as_str()), ("B", "x"));

        assert!(parse_roleplay_scorecard(&json!({}), "", "").is_none());
        assert!(parse_roleplay_scorecard(&json!("nope"), "", "").is_none());
        assert!(parse_roleplay_scorecard(&json!({"overall_score": "high"}), "", "").is_none());
        assert!(parse_roleplay_scorecard(&json!({"strengths": "calm"}), "", "").is_none());
    }

    #[test]
    fn heuristic_scorecard_scores_from_keywords() {
        let mut s = session(vec![
            turn(
                1,
                "Good morning, Slater Byrne Recoveries, how can I help?",
                "",
                "",
            ),
            turn(
                2,
                "I understand. There is no joining fee, commission only.",
                "",
                "",
            ),
        ]);
        s.mode = "roleplay".into();
        s.roleplay_scenario = "fees".into();
        let card = heuristic_roleplay_scorecard(&s);
        assert_eq!(
            (
                card.greeting,
                card.empathy,
                card.discovery,
                card.accuracy,
                card.control
            ),
            (8, 8, 3, 9, 6)
        );
        assert_eq!(card.overall_score, 68);
        assert_eq!(card.grade, "C");
        assert_eq!(card.scenario_label, "Fees");
        assert_eq!(
            card.strengths,
            [
                "Professional opening",
                "SOP facts landed correctly",
                "Showed empathy"
            ]
        );
        assert_eq!(card.improvements, ["Ask debt type and amount early"]);
        assert!(card
            .coach_notes
            .starts_with("Practice scenario: Fees. Offline score"));
        assert!(card.coach_notes.contains("Staff turns heard: 2."));

        let mut silent = session(vec![]);
        silent.roleplay_scenario = "angry_customer".into();
        let card = heuristic_roleplay_scorecard(&silent);
        assert_eq!(
            (
                card.greeting,
                card.empathy,
                card.discovery,
                card.accuracy,
                card.control
            ),
            (2, 4, 3, 5, 4)
        );
        assert_eq!((card.overall_score, card.grade.as_str()), (36, "F"));
        assert_eq!(card.scenario_label, "Angry Customer");
        assert_eq!(card.strengths, ["Completed practice call"]);
    }

    #[test]
    fn action_items_and_stakeholders_are_parsed_leniently() {
        let items = parse_action_items(&json!([
            {"owner": "caller", "task": " send invoice ", "due": "same_day"},
            {"task": "call back", "due": ""},
            {"owner": "staff", "task": "  "},
            "junk",
            {"task": "x", "due": 3}
        ]));
        assert_eq!(items.len(), 3);
        assert_eq!(
            (
                items[0].owner.as_str(),
                items[0].task.as_str(),
                items[0].due.as_deref()
            ),
            ("caller", "send invoice", Some("same_day"))
        );
        assert_eq!(
            (items[1].owner.as_str(), items[1].due.as_deref()),
            ("staff", None)
        );
        assert_eq!(items[2].due.as_deref(), Some("3"));
        assert!(parse_action_items(&json!({"task": "x"})).is_empty());

        let people = parse_stakeholders(
            &json!([{"role": "debtor", "name": "Acme", "notes": "owes"}, {"name": ""}, 5]),
        );
        assert_eq!(people.len(), 2);
        assert_eq!(
            (
                people[0].role.as_str(),
                people[0].name.as_deref(),
                people[0].notes.as_str()
            ),
            ("debtor", Some("Acme"), "owes")
        );
        assert_eq!(
            (
                people[1].role.as_str(),
                people[1].name.as_deref(),
                people[1].notes.as_str()
            ),
            ("caller", None, "")
        );
    }

    #[test]
    fn heuristic_outcomes_follow_the_action() {
        let cases = [
            (
                "transfer",
                "transferred",
                "transfer",
                "Take over call / call back customer after transfer",
            ),
            (
                "callback",
                "callback_requested",
                "callback",
                "Schedule callback with caller",
            ),
            (
                "decline_debt_type",
                "declined_unsupported",
                "decline_debt_type",
                "Use free Letter of Demand template on website",
            ),
            (
                "complete",
                "completed",
                "complete",
                "Review enquiry and follow up if needed",
            ),
            (
                "something_else",
                "active",
                "follow_up",
                "Review enquiry and follow up if needed",
            ),
        ];
        for (action, outcome, next, task) in cases {
            let mut s = session(vec![turn(1, "hello there", "new_enquiry", "ok")]);
            s.status = "active".into();
            s.latest_action = Some(action.into());
            let a = heuristic_post_analysis(&s);
            assert_eq!(a.outcome, outcome, "{action}");
            assert_eq!(a.next_best_action, next, "{action}");
            assert_eq!(a.action_items[0].task, task, "{action}");
            assert_eq!(a.status, "ready");
        }
        let mut s = session(vec![]);
        s.status = String::new();
        assert_eq!(heuristic_post_analysis(&s).outcome, "ended");
        s.status = "completed".into();
        assert_eq!(heuristic_post_analysis(&s).outcome, "completed");
    }

    #[test]
    fn heuristic_analysis_of_a_transfer_call() {
        let mut s = session(vec![
            turn(0, "", "opening_greeting", "Welcome"),
            turn(1, " I need help with an invoice ", "new_enquiry", "Sure"),
            turn(
                2,
                "Put me through to a person",
                "realtime_utterance",
                "Transferring",
            ),
        ]);
        s.latest_intent = "realtime_utterance".into();
        s.slots = slots(
            json!({"debt_type": "invoice", "amount": "", "staff_name": "null", "action": "transfer"}),
        );
        let a = heuristic_post_analysis(&s);
        assert_eq!(a.primary_intent, "request_transfer");
        assert_eq!(a.outcome, "transferred");
        assert_eq!(a.turn_intents, ["new_enquiry", "realtime_utterance"]);
        assert_eq!(a.risks, ["no_amount", "transfer_requested"]);
        assert_eq!(
            Value::Object(a.key_facts.clone()),
            json!({"debt_type": "invoice", "action": "transfer"})
        );
        assert_eq!(
            a.summary,
            "Caller discussed: I need help with an invoice · Put me through to a person. Debt type noted as invoice. Outcome: transferred."
        );
        assert_eq!(a.caller_goal, "I need help with an invoice");
        assert_eq!(
            a.internal_handoff_brief,
            "Caller goal: I need help with an invoice\nDebt type: invoice\nAmount: not stated\nRecommended action: transfer\nOutcome so far: transferred"
        );
        assert!(a.follow_up_email_draft.starts_with(
            "Subject: Following up on your enquiry\n\nHello,\n\nThank you for calling Slater Byrne Recoveries. Regarding your enquiry (invoice): Caller discussed:"
        ));
        assert!(a.follow_up_email_draft.ends_with(
            "Please reply if you need anything further.\n\nKind regards,\nEnquiries team"
        ));
        assert_eq!(
            (
                a.stakeholders[0].role.as_str(),
                a.stakeholders[0].notes.as_str()
            ),
            ("caller", "I need help with an invoice")
        );
        assert!(a.roleplay_scorecard.is_none() && a.error.is_empty());
        assert_eq!(a.recovered_slots, s.slots);
    }

    #[test]
    fn heuristic_risks_for_unsupported_and_empty_calls() {
        let mut s = session(vec![]);
        s.latest_intent = "unsupported_debt".into();
        s.slots = slots(json!({"amount": 1200}));
        let a = heuristic_post_analysis(&s);
        assert_eq!(a.risks, ["unsupported_debt", "incomplete_details"]);
        assert_eq!(a.caller_goal, "Not clearly captured");
        assert_eq!(a.summary, "Amount noted as 1200. Outcome: completed.");
        assert!(a
            .follow_up_email_draft
            .contains("(not stated, amount 1200)"));
        assert!(a
            .internal_handoff_brief
            .starts_with("Caller goal: unclear\n"));

        let mut many = session(
            (1..=4)
                .map(|n| turn(n, &format!("bit{n}"), "", ""))
                .collect(),
        );
        many.slots = slots(json!({"amount": "$5"}));
        let a = heuristic_post_analysis(&many);
        assert!(a
            .summary
            .starts_with("Caller discussed: bit1 · bit2 · bit3…. Amount noted as $5."));
        assert!(a.risks.is_empty());
        assert_eq!(a.primary_intent, "unknown");
    }

    #[test]
    fn heuristic_analysis_of_a_roleplay_call() {
        let mut s = session(vec![
            turn(0, "", "roleplay_customer_opening", "Hi"),
            turn(1, "Hello, how can I help", "x", "ok"),
        ]);
        s.mode = "roleplay".into();
        s.roleplay_scenario = "fees".into();
        let a = heuristic_post_analysis(&s);
        assert_eq!(a.primary_intent, "roleplay_fees");
        assert_eq!(a.outcome, "practice_completed");
        assert_eq!(a.next_best_action, "follow_up");
        assert_eq!(a.summary, "Roleplay training (fees). Staff said: Hello, how can I help. Outcome: practice completed.");
        assert_eq!(a.key_facts.get("roleplay_scenario"), Some(&json!("fees")));
        assert_eq!(
            a.action_items[0].task,
            "Review roleplay scorecard and retry weak scenarios"
        );
        assert_eq!(a.stakeholders[0].role, "trainee");
        assert_eq!(a.follow_up_email_draft, "");
        assert_eq!(a.caller_goal, "Practice scenario: Fees");
        let card = a.roleplay_scorecard.unwrap();
        assert!(a.internal_handoff_brief.starts_with(&format!(
            "ROLEPLAY SCORECARD\nScenario: Fees\nScore: {}/100 ({})\nStrengths: ",
            card.overall_score, card.grade
        )));
        assert!(!a.risks.contains(&"no_amount".to_string()));

        s.turn_count = 1;
        assert_eq!(heuristic_post_analysis(&s).outcome, "practice_partial");
    }

    #[test]
    fn model_answer_is_merged_with_fallbacks() {
        let mut s = session(vec![turn(1, "I am owed money", "new_enquiry", "ok")]);
        s.slots = slots(json!({"debt_type": "invoice", "amount": 100}));
        let content = json!({
            "primary_intent": "ask_fees",
            "summary": "  Caller asked about fees.  ",
            "recovered_slots": {"amount": 250, "reference": "", "staff_name": null, "action": "callback"},
            "risks": ["no_reference", "", null],
            "action_items": [{"task": "Email fee sheet"}],
        })
        .to_string();
        let a = post_analysis_from_model(&s, &content).unwrap();
        assert_eq!(a.primary_intent, "ask_fees");
        assert_eq!(a.summary, "Caller asked about fees.");
        assert_eq!(a.outcome, "completed");
        assert_eq!(
            Value::Object(a.recovered_slots.clone()),
            json!({"debt_type": "invoice", "amount": 250, "action": "callback"})
        );
        assert_eq!(a.risks, ["no_reference"]);
        assert_eq!(a.action_items.len(), 1);
        assert_eq!(a.next_best_action, "callback");
        assert_eq!(a.caller_goal, "I am owed money");
        assert_eq!(a.key_facts.get("debt_type"), Some(&json!("invoice")));
        assert!(a.follow_up_email_draft.starts_with("Subject: Following up"));
        assert!(a
            .internal_handoff_brief
            .starts_with("Caller goal: I am owed money"));
        assert_eq!(a.stakeholders[0].role, "caller");
        assert_eq!(a.turn_intents, ["new_enquiry"]);

        let empty = post_analysis_from_model(&s, "{}").unwrap();
        assert_eq!(empty.primary_intent, "unknown");
        assert_eq!(empty.next_best_action, "follow_up");
        assert!(!empty.summary.is_empty() && !empty.action_items.is_empty());

        assert!(post_analysis_from_model(&s, "[1]").is_err());
        assert!(post_analysis_from_model(&s, "not json").is_err());
    }

    #[test]
    fn model_answer_for_roleplay_uses_scorecard() {
        let mut s = session(vec![turn(1, "Hello", "", "Hi")]);
        s.mode = "roleplay".into();
        s.roleplay_scenario = "transfer".into();
        s.roleplay_label = "Transfer request".into();
        let content = json!({"summary": "Good call", "roleplay_scorecard": {"overall_score": 72, "greeting": 11}}).to_string();
        let a = post_analysis_from_model(&s, &content).unwrap();
        let card = a.roleplay_scorecard.clone().unwrap();
        assert_eq!(
            (card.overall_score, card.grade.as_str(), card.greeting),
            (72, "B", 10)
        );
        assert_eq!(
            (card.scenario.as_str(), card.scenario_label.as_str()),
            ("transfer", "Transfer request")
        );
        assert_eq!(a.follow_up_email_draft, "");
        assert!(a.internal_handoff_brief.starts_with("ROLEPLAY SCORECARD"));

        let a = post_analysis_from_model(&s, r#"{"roleplay_scorecard": "bad"}"#).unwrap();
        assert!(a
            .roleplay_scorecard
            .unwrap()
            .coach_notes
            .contains("Offline score"));
    }

    #[test]
    fn content_pack_has_five_artifacts_and_a_conditional_checklist() {
        let mut s = session(vec![turn(1, "I am owed rent", "new_enquiry", "ok")]);
        s.latest_transcript = "I am owed rent".into();
        s.latest_response = "ok".into();
        s.slots = slots(json!({"debt_type": "invoice"}));
        let analysis = CallPostAnalysis {
            primary_intent: "new_enquiry".into(),
            summary: "Caller asked about an invoice.".into(),
            outcome: "completed".into(),
            caller_goal: "recovering an invoice".into(),
            recovered_slots: slots(json!({"amount": 900})),
            next_best_action: "callback".into(),
            ..CallPostAnalysis::default()
        };
        let pack = generate_call_content(&s, Some(&analysis));
        assert_eq!(pack.status, "ready");
        let keys: Vec<&str> = pack.artifacts.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "follow_up_email",
                "handoff_brief",
                "follow_up_sms",
                "fee_faq",
                "enquiry_log_json"
            ]
        );
        let types: Vec<&str> = pack
            .artifacts
            .iter()
            .map(|a| a.content_type.as_str())
            .collect();
        assert_eq!(types, ["email", "text", "sms", "text", "text"]);
        assert_eq!(
            pack.artifacts[0].body,
            "Subject: Following up on your enquiry\n\nHello,\n\nThank you for calling Slater Byrne Recoveries regarding recovering an invoice.\n\nCaller asked about an invoice.\n\nDebt type: invoice\nAmount: 900\n\nPlease reply if you need anything further.\n\nKind regards,\nEnquiries team"
        );
        assert_eq!(
            pack.artifacts[1].body,
            "INTERNAL HANDOFF BRIEF\nCall: s1\nIntent: new_enquiry\nAction: callback\nDebt type: invoice\nAmount: 900\nCaller last said: I am owed rent\nAI last said: ok\nPlease greet the caller and continue intake."
        );
        assert_eq!(
            pack.artifacts[2].body,
            "Hi, thanks for calling Slater Byrne Recoveries about invoice (900). We'll follow up as discussed. Reply STOP to opt out."
        );
        assert_eq!(
            pack.artifacts[3].body,
            "Fees at Slater Byrne Recoveries:\n- No joining fee\n- No account maintenance fee\n- Commission only if we successfully recover the debt\nSuitable for a short verbal answer or email insert."
        );
        let log: Value = serde_json::from_str(&pack.artifacts[4].body).unwrap();
        assert_eq!(
            log,
            json!({"session_id": "s1", "status": "completed", "primary_intent": "new_enquiry", "outcome": "completed",
                   "slots": {"debt_type": "invoice", "amount": 900}, "summary": "Caller asked about an invoice.",
                   "next_best_action": "callback", "risks": []})
        );
        assert!(pack.artifacts[4]
            .body
            .starts_with("{\n  \"session_id\": \"s1\",\n  \"status\""));

        for analysis in [
            CallPostAnalysis {
                next_best_action: "decline_debt_type".into(),
                ..CallPostAnalysis::default()
            },
            CallPostAnalysis {
                primary_intent: "Unsupported_debt".into(),
                ..CallPostAnalysis::default()
            },
            CallPostAnalysis {
                risks: vec!["UNSUPPORTED type".into()],
                ..CallPostAnalysis::default()
            },
        ] {
            let pack = generate_call_content(&s, Some(&analysis));
            assert_eq!(pack.artifacts.len(), 6);
            let lod = &pack.artifacts[4];
            assert_eq!(
                (lod.key.as_str(), lod.content_type.as_str()),
                ("unsupported_lod_checklist", "checklist")
            );
            assert!(lod.body.starts_with("UNSUPPORTED DEBT — Letter of Demand checklist\nDebt type noted: invoice\n1. Explain"));
            assert!(lod.body.ends_with("5. Offer transfer only if they insist on speaking to a person about other matters."));
            assert_eq!(pack.artifacts[5].key, "enquiry_log_json");
        }
    }

    #[test]
    fn content_pack_prefers_existing_drafts_and_falls_back_to_heuristic() {
        let mut s = session(vec![turn(1, "hello", "", "hi")]);
        s.handoff_brief = " Stored brief ".into();
        let analysis = CallPostAnalysis {
            follow_up_email_draft: " Dear caller ".into(),
            internal_handoff_brief: "analysis brief".into(),
            ..CallPostAnalysis::default()
        };
        let pack = generate_call_content(&s, Some(&analysis));
        assert_eq!(pack.artifacts[0].body, "Dear caller");
        assert_eq!(pack.artifacts[1].body, "Stored brief");
        assert_eq!(
            pack.artifacts[2].body,
            "Hi, thanks for calling Slater Byrne Recoveries about not stated. We'll follow up as discussed. Reply STOP to opt out."
        );

        s.handoff_brief.clear();
        s.post_analysis = Some(analysis);
        assert_eq!(
            generate_call_content(&s, None).artifacts[1].body,
            "analysis brief"
        );
        s.post_analysis = None;
        let pack = generate_call_content(&s, None);
        assert!(pack.artifacts[1].body.starts_with("Caller goal: hello"));
    }

    #[test]
    fn prompts_match_the_python_source() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let checks = [
            (
                "voice_in_agent/service.py",
                vec![
                    POST_ANALYSIS_PROMPT,
                    ROLEPLAY_ANALYSIS_PROMPT,
                    COLLECTIONS_ANALYSIS_PROMPT,
                ],
            ),
            (
                "voice_mvp/xai_client.py",
                vec![crate::prompts::DEVICE_SYSTEM_PROMPT],
            ),
        ];
        for (file, prompts) in checks {
            let Ok(source) = std::fs::read_to_string(root.join(file)) else {
                continue;
            };
            let source = source.replace("\r\n", "\n");
            for prompt in prompts {
                assert!(prompt.len() > 100 && prompt.ends_with('\n'));
                assert!(
                    source.contains(&format!("\"\"\"{prompt}\"\"\"")),
                    "prompt drifted from {file}"
                );
            }
        }
    }
}
