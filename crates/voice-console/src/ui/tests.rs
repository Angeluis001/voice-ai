use serde_json::json;

use super::py::{json_str, json_str_dict, py_str, take_chars, title};
use super::*;

fn session(fields: Value) -> LiveCallSession {
    let mut obj = json!({"session_id": "s1"});
    obj.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(obj).unwrap()
}

#[test]
fn escape_matches_python_html_escape() {
    assert_eq!(
        escape(r#"<a href="x">Tom & 'Jerry'</a>"#),
        "&lt;a href=&quot;x&quot;&gt;Tom &amp; &#x27;Jerry&#x27;&lt;/a&gt;"
    );
    assert_eq!(escape("plain — text"), "plain — text");
}

#[test]
fn python_string_helpers() {
    assert_eq!(title("some custom phase 2nd"), "Some Custom Phase 2Nd");
    assert_eq!(title("debt type"), "Debt Type");
    assert_eq!(py_str(&json!("x")), "x");
    assert_eq!(py_str(&json!(6000)), "6000");
    assert_eq!(py_str(&json!(1250.5)), "1250.5");
    assert_eq!(py_str(&json!(false)), "False");
    assert_eq!(py_str(&json!(["a", "it's"])), r#"['a', "it's"]"#);
    assert_eq!(json_str("/ops"), r#""/ops""#);
    assert_eq!(
        json_str("English — UK \"x\""),
        "\"English \\u2014 UK \\\"x\\\"\""
    );
    assert_eq!(
        json_str_dict(&[("en", "English"), ("fr", "French")]),
        r#"{"en": "English", "fr": "French"}"#
    );
    assert_eq!(json_str_dict(&[]), "{}");
    assert_eq!(take_chars("héllo", 2), "hé");
    assert_eq!(take_chars("hi", 5), "hi");
}

#[test]
fn path_adds_the_base_prefix() {
    let local = UiContext::default();
    assert_eq!(local.path("/console"), "/console");
    assert_eq!(local.path("/"), "/");
    assert_eq!(local.path(""), "/");
    let ops = UiContext {
        base_path: "/ops/".into(),
        ..Default::default()
    };
    assert_eq!(ops.path("/console"), "/ops/console");
    assert_eq!(ops.path("console"), "/ops/console");
    assert_eq!(ops.path("/"), "/ops/");
    assert_eq!(ops.path(""), "/ops");
}

#[test]
fn phase_labels() {
    assert_eq!(phase_label(Some("queued"), None), "Processing…");
    assert_eq!(phase_label(Some(" Processing "), None), "Processing…");
    assert_eq!(phase_label(Some("transcribed"), None), "Caller heard");
    assert_eq!(phase_label(Some("reasoned"), None), "Reply ready");
    assert_eq!(phase_label(Some("completed"), None), "Spoken");
    assert_eq!(phase_label(Some("opening_greeting"), None), "Greeting");
    assert_eq!(phase_label(Some("skipped"), None), "Skipped");
    assert_eq!(phase_label(Some(""), Some("failed")), "Failed");
    assert_eq!(
        phase_label(None, Some("callback_requested")),
        "Callback Requested"
    );
    assert_eq!(phase_label(Some("reasoned"), Some("failed")), "Reply ready");
    assert_eq!(phase_label(None, None), "Idle");
    assert_eq!(phase_label(Some(""), Some("")), "Idle");
}

#[test]
fn line_labels() {
    assert_eq!(line_label(None, None, None), "—");
    assert_eq!(line_label(Some("9198"), None, None), "9198 · Enquiries");
    assert_eq!(
        line_label(Some("2900"), Some("sop_enquiries"), None),
        "2900 · Enquiries"
    );
    assert_eq!(
        line_label(None, Some("sop_enquiries_realtime"), None),
        "— · Enquiries"
    );
    assert_eq!(line_label(Some("9197"), None, None), "9197 · AI Pilot");
    assert_eq!(line_label(Some("9199"), None, None), "9199 · Roleplay");
    assert_eq!(line_label(Some("0400"), Some("pilot"), None), "0400");

    let rp = session(
        json!({"mode": "roleplay", "roleplay_label": "Hardship", "roleplay_scenario": "hardship"}),
    );
    assert_eq!(
        line_label(Some("9198"), None, Some(&rp)),
        "9199 · Roleplay · Hardship"
    );
    let rp = session(json!({"prompt_profile": "ROLEPLAY", "roleplay_scenario": "gate"}));
    assert_eq!(line_label(None, None, Some(&rp)), "9199 · Roleplay · gate");
    let rp = session(json!({"target_number": "9199"}));
    assert_eq!(
        line_label(Some("9199"), None, Some(&rp)),
        "9199 · Roleplay · practice"
    );
    let normal = session(json!({"target_number": "9198"}));
    assert_eq!(
        line_label(Some("9198"), None, Some(&normal)),
        "9198 · Enquiries"
    );
}

#[test]
fn session_pills() {
    let pill = |fields: Value| session_pill(&session(fields));
    assert_eq!(
        pill(json!({"status": "transferred", "is_active": true})),
        ("transferred", "Transferred")
    );
    assert_eq!(
        pill(json!({"status": "callback_requested"})),
        ("callback", "Callback")
    );
    assert_eq!(
        pill(json!({"status": " Completed "})),
        ("completed", "Ended")
    );
    assert_eq!(
        pill(json!({"status": "failed", "latest_phase": "processing"})),
        ("completed", "Failed")
    );
    assert_eq!(
        pill(json!({"status": "idle", "latest_phase": "Transcribed"})),
        ("live", "Live")
    );
    assert_eq!(pill(json!({"is_active": true})), ("live", "Live"));
    assert_eq!(pill(json!({"status": "idle"})), ("recent", "Recent"));
}

#[test]
fn slots_line() {
    assert_eq!(format_slots(None), "");
    let slots = json!({"reference": "R1", "staff_name": "unknown", "amount": 6000, "debt_type": "invoice", "other": "x"});
    assert_eq!(
        format_slots(slots.as_object()),
        "Debt: invoice · Amount: 6000 · Ref: R1"
    );
    let blank =
        json!({"debt_type": null, "amount": "", "staff_name": "null", "reference": "unknown"});
    assert_eq!(format_slots(blank.as_object()), "");
    assert_eq!(
        format_slots(json!({"amount": 0, "staff_name": "Alex"}).as_object()),
        "Amount: 0 · Staff: Alex"
    );
}

#[test]
fn assist_rules_apply_in_order() {
    let title_of = |fields: Value| assist_text(&session(fields)).0;

    // 1. roleplay wins over everything
    let (title, body, tips, risk) = assist_text(&session(
        json!({"mode": "roleplay", "latest_action": "transfer", "roleplay_scenario": "gate"}),
    ));
    assert_eq!(title, "Roleplay · you are the agent");
    assert_eq!(
        body,
        "Scenario: gate. AI is the customer — practice SOP intake."
    );
    assert_eq!(tips.len(), 3);
    assert_eq!(risk, "");

    // 2. transfer (by action, by slot action, by intent) beats callback
    let (title, _, tips, risk) = assist_text(&session(
        json!({"latest_action": "Transfer", "latest_intent": "callback"}),
    ));
    assert_eq!(
        (title.as_str(), tips.len(), risk.as_str()),
        ("Transfer ready", 2, "transfer requested")
    );
    assert_eq!(
        title_of(json!({"slots": {"action": "transfer"}})),
        "Transfer ready"
    );
    assert_eq!(
        title_of(json!({"latest_intent": "Transfer_Request"})),
        "Transfer ready"
    );
    // the explicit action hides the slot action
    assert_eq!(
        title_of(json!({"latest_action": "continue", "slots": {"action": "transfer"}})),
        "Standby"
    );

    // 3. callback beats unsupported
    assert_eq!(
        title_of(json!({"latest_action": "callback", "latest_intent": "unsupported_debt"})),
        "Callback"
    );
    assert_eq!(
        title_of(json!({"latest_intent": "callback_request"})),
        "Callback"
    );

    // 4. unsupported beats fees
    let (title, _, _, risk) = assist_text(&session(
        json!({"latest_action": "decline_debt_type", "latest_intent": "fees"}),
    ));
    assert_eq!(
        (title.as_str(), risk.as_str()),
        ("Unsupported debt type", "unsupported debt")
    );
    assert_eq!(
        title_of(json!({"latest_intent": "unsupported_debt"})),
        "Unsupported debt type"
    );

    // 5. fees beats the slot rules
    assert_eq!(
        title_of(json!({"latest_intent": "fees_question", "slots": {"debt_type": "loan"}})),
        "Fees"
    );

    // 6. debt without amount
    let (title, body, _, risk) = assist_text(&session(
        json!({"is_active": true, "slots": {"debt_type": "loan", "amount": ""}}),
    ));
    assert_eq!(title, "Amount missing");
    assert_eq!(body, "Debt type noted as loan. Still need amount owed.");
    assert_eq!(risk, "no amount");
    assert_eq!(
        title_of(json!({"slots": {"debt_type": "loan", "amount": 0}})),
        "Amount missing"
    );

    // 7. debt and amount
    let (title, body, _, risk) = assist_text(&session(
        json!({"is_active": true, "slots": {"debt_type": "loan", "amount": 1250.5}}),
    ));
    assert_eq!(title, "Intake in progress");
    assert_eq!(
        body,
        "Debt: loan · Amount: 1250.5. Continue qualification or transfer if requested."
    );
    assert_eq!(risk, "");

    // 8. live call with nothing else to say
    let (title, _, tips, _) = assist_text(&session(
        json!({"is_active": true, "slots": {"amount": 300}}),
    ));
    assert_eq!((title.as_str(), tips.len()), ("Live enquiry", 1));

    // 9. standby
    let (title, body, tips, risk) = assist_text(&session(json!({})));
    assert_eq!(title, "Standby");
    assert_eq!(body, "No active coaching signal for this session.");
    assert!(tips.is_empty() && risk.is_empty());
}

#[test]
fn sidebar_footer_shows_the_signed_in_user() {
    let anon = render_home(&UiContext::default(), &[]);
    assert!(anon.contains("Slater Byrne Recoveries<br/>Formal operations console · v1"));
    let ctx = UiContext {
        base_path: "/ops".into(),
        user: Some(UiUser {
            name: "A <b>".into(),
            email: "a@example.test".into(),
        }),
        auth_enabled: true,
    };
    let html = render_home(&ctx, &[]);
    assert!(html.contains(
        r#"<div class="user-name">A &lt;b&gt;</div><div class="user-email">a@example.test</div>"#
    ));
    assert!(html.contains(r#"<a class="user-logout" href="/ops/auth/logout">Sign out</a>"#));
}

#[test]
fn runtime_values_are_injected_into_the_scripts() {
    let ctx = UiContext {
        base_path: "/ops".into(),
        ..Default::default()
    };
    assert!(render_console(&ctx, &[])
        .contains("const BASE_PATH = \"/ops\";\n    function P(u) { return BASE_PATH + u; }"));
    let brand = render_brand(&ctx, &[("en", "English"), ("en-AU", "English — Australia")]);
    assert!(brand.contains("const API = \"/ops/api/brand\";\nconst LANG_LABELS = {\"en\": \"English\", \"en-AU\": \"English \\u2014 Australia\"};\n"));
    assert!(brand.contains(
        r#"<option value="en">English</option><option value="en-AU">English — Australia</option>"#
    ));
    assert!(!brand.contains("{{ACCENT_OPTIONS}}"));
}
