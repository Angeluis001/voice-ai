//! Compares every rendered page with the HTML the original Python UI produced
//! for the same synthetic sessions (see `tests/golden/make_golden.py`).
use std::path::PathBuf;

use serde_json::Value;
use voice_console::ui::{self, UiContext, UiUser};
use voice_sessions::models::{CallPostAnalysis, LiveCallSession};

struct Fixtures {
    sessions: Vec<LiveCallSession>,
    pack: Value,
    pack_no_tree: Value,
    lang_labels: Vec<(String, String)>,
    user: UiUser,
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn load_fixtures() -> Fixtures {
    let text = std::fs::read_to_string(golden_dir().join("fixtures.json")).expect("fixtures.json");
    let root: Value = serde_json::from_str(&text).expect("fixtures parse");
    Fixtures {
        sessions: serde_json::from_value(root["sessions"].clone()).expect("sessions"),
        pack: root["pack"].clone(),
        pack_no_tree: root["pack_no_tree"].clone(),
        lang_labels: serde_json::from_value(root["lang_labels"].clone()).expect("lang_labels"),
        user: UiUser {
            name: root["user"]["name"].as_str().unwrap().into(),
            email: root["user"]["email"].as_str().unwrap().into(),
        },
    }
}

fn render_all(base_path: &str, fx: &Fixtures) -> Vec<(String, String)> {
    let ctx = UiContext {
        base_path: base_path.into(),
        user: None,
        auth_enabled: false,
    };
    let labels: Vec<(&str, &str)> = fx
        .lang_labels
        .iter()
        .map(|(c, l)| (c.as_str(), l.as_str()))
        .collect();
    let all = fx.sessions.clone();
    let ended: Vec<LiveCallSession> = all.iter().filter(|s| !s.is_active).cloned().collect();

    let mut pages: Vec<(String, String)> = vec![
        ("login".into(), ui::render_login(&ctx, "")),
        (
            "login_error".into(),
            ui::render_login(&ctx, "Sign-in <failed> & 'expired'"),
        ),
        ("brand".into(), ui::render_brand(&ctx, &labels)),
        ("train_pack".into(), ui::render_train(&ctx, &all, &fx.pack)),
        (
            "train_pack_no_tree".into(),
            ui::render_train(&ctx, &[], &fx.pack_no_tree),
        ),
    ];
    for (set_name, subset) in [
        ("all", all.as_slice()),
        ("ended", ended.as_slice()),
        ("empty", &[]),
    ] {
        pages.push((format!("home_{set_name}"), ui::render_home(&ctx, subset)));
        pages.push((
            format!("console_{set_name}"),
            ui::render_console(&ctx, subset),
        ));
        pages.push((
            format!("assist_{set_name}"),
            ui::render_assist(&ctx, subset),
        ));
        pages.push((
            format!("record_{set_name}"),
            ui::render_record(&ctx, subset),
        ));
        pages.push((
            format!("capture_{set_name}"),
            ui::render_capture(&ctx, subset),
        ));
        pages.push((
            format!("train_{set_name}"),
            ui::render_train(&ctx, subset, &Value::Null),
        ));
        pages.push((
            format!("create_{set_name}"),
            ui::render_create(&ctx, subset),
        ));
    }
    for (i, s) in all.iter().enumerate() {
        let analysis = s.post_analysis.clone().unwrap_or_default();
        pages.push((
            format!("summary_{i:02}"),
            ui::render_summary(&ctx, s, &analysis),
        ));
    }
    let signed_in = UiContext {
        user: Some(fx.user.clone()),
        auth_enabled: true,
        ..ctx.clone()
    };
    pages.push(("home_user".into(), ui::render_home(&signed_in, &all)));
    let blank = UiContext {
        user: Some(UiUser::default()),
        ..signed_in
    };
    pages.push(("home_user_blank".into(), ui::render_home(&blank, &[])));
    pages
}

/// Where two strings first differ, with some context, for a readable failure.
fn first_diff(expected: &str, actual: &str) -> String {
    let at = expected
        .bytes()
        .zip(actual.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(actual.len()));
    let window = |s: &str| {
        let mut start = at.saturating_sub(120);
        while !s.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (at + 120).min(s.len());
        while !s.is_char_boundary(end) {
            end += 1;
        }
        s[start..end].to_string()
    };
    format!(
        "first difference at byte {at} (python {} bytes, rust {} bytes)\n--- python ---\n{:?}\n--- rust ---\n{:?}",
        expected.len(),
        actual.len(),
        window(expected),
        window(actual)
    )
}

fn check_variant(variant: &str, base_path: &str) {
    let fx = load_fixtures();
    let dir = golden_dir().join(variant);
    let pages = render_all(base_path, &fx);
    let golden_count = std::fs::read_dir(&dir).expect("golden dir").count();
    assert_eq!(
        pages.len(),
        golden_count,
        "page list is out of step with {}",
        dir.display()
    );
    let mut failures = Vec::new();
    for (name, html) in &pages {
        let expected = std::fs::read_to_string(dir.join(format!("{name}.html")))
            .unwrap_or_else(|e| panic!("{variant}/{name}.html: {e}"))
            .replace("\r\n", "\n");
        if *html != expected {
            failures.push(format!("{variant}/{name}: {}", first_diff(&expected, html)));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} pages differ:\n\n{}",
        failures.len(),
        pages.len(),
        failures.join("\n\n")
    );
}

#[test]
fn pages_match_python_at_root() {
    check_variant("root", "");
}

#[test]
fn pages_match_python_behind_ops_prefix() {
    check_variant("ops", "/ops");
}

#[test]
fn fixtures_cover_the_interesting_cases() {
    let fx = load_fixtures();
    assert!(fx
        .sessions
        .iter()
        .any(|s| s.is_active && !ui::is_roleplay(s)));
    assert!(fx
        .sessions
        .iter()
        .any(|s| s.status == "transferred" && s.content.is_some() && !s.handoff_brief.is_empty()));
    assert!(fx.sessions.iter().any(|s| {
        ui::is_roleplay(s)
            && s.post_analysis
                .as_ref()
                .is_some_and(|a: &CallPostAnalysis| a.roleplay_scorecard.is_some())
    }));
}
