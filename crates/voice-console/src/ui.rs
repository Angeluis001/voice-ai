//! Server-rendered console pages. A line-for-line port of the original Python
//! `console_ui.py`: the HTML, CSS and JS must stay byte-identical because the
//! browser scripts depend on the markup (see `tests/golden.rs`).
//!
//! The raw string templates below keep the original indentation on purpose.
use serde_json::{Map, Value};
use voice_sessions::models::{CallPostAnalysis, LiveCallSession};

mod py;

pub use py::escape;
use py::{is_blank_fact, json_str, json_str_dict, or, py_str, take_chars, title, truthy};

const BASE_CSS: &str = include_str!("../static/base.css");
const LOGIN_CSS: &str = include_str!("../static/login.css");
const BRAND_CSS: &str = include_str!("../static/brand.css");
const CONSOLE_JS: &str = include_str!("../static/console.js");
const BRAND_JS: &str = include_str!("../static/brand.js");
const BRAND_BODY: &str = include_str!("../static/brand_body.html");
const COPY_CREATE_JS: &str = include_str!("../static/copy_create.js");
const COPY_SUMMARY_JS: &str = include_str!("../static/copy_summary.js");

#[derive(Debug, Clone, Default)]
pub struct UiUser {
    pub name: String,
    pub email: String,
}

/// Per-request context. `base_path` is "" locally or e.g. "/ops" behind a proxy.
#[derive(Debug, Clone, Default)]
pub struct UiContext {
    pub base_path: String,
    /// The signed-in user, shown in the sidebar footer. `None` shows the default footer.
    pub user: Option<UiUser>,
    /// Not used by the pages themselves; carried for the web layer.
    pub auth_enabled: bool,
}

impl UiContext {
    fn base(&self) -> &str {
        self.base_path.trim_end_matches('/')
    }

    /// Prefix an app-relative path with the base path (e.g. /console -> /ops/console).
    pub fn path(&self, p: &str) -> String {
        let base = self.base();
        if p.is_empty() {
            return if base.is_empty() {
                "/".into()
            } else {
                base.into()
            };
        }
        if p.starts_with('/') {
            format!("{base}{p}")
        } else {
            format!("{base}/{p}")
        }
    }
}

// ---------------------------------------------------------------------------
// Navigation / skills
// ---------------------------------------------------------------------------

struct Skill {
    id: &'static str,
    href: &'static str,
    label: &'static str,
    nav: &'static str,
    tagline: &'static str,
    desc: &'static str,
    icon: &'static str,
    cta: Option<&'static str>,
}

const SKILLS: &[Skill] = &[
    Skill {
        id: "home",
        href: "/",
        label: "Home",
        nav: "Overview",
        tagline: "Command centre",
        desc: "Select a capability and open a focused workspace.",
        icon: "◆",
        cta: None,
    },
    Skill {
        id: "console",
        href: "/console",
        label: "Live Console",
        nav: "Live Console",
        tagline: "Real-time operations",
        desc: "Watch live calls as they happen — speech, intent, slots, and replies.",
        icon: "◎",
        cta: Some("Open live board"),
    },
    Skill {
        id: "assist",
        href: "/assist",
        label: "Assist",
        nav: "Assist",
        tagline: "Ride-along coaching",
        desc: "Operator guidance cards while the AI or staff handle enquiries.",
        icon: "◇",
        cta: Some("Open assist"),
    },
    Skill {
        id: "record",
        href: "/record",
        label: "Record",
        nav: "Record",
        tagline: "Call library",
        desc: "Playback mixed recordings of caller and AI conversations.",
        icon: "◉",
        cta: Some("Open library"),
    },
    Skill {
        id: "capture",
        href: "/capture",
        label: "Capture",
        nav: "Capture",
        tagline: "Summary Inform",
        desc: "Post-call intent, facts, risks, and structured recovery of the conversation.",
        icon: "▣",
        cta: Some("Open capture"),
    },
    Skill {
        id: "train",
        href: "/train",
        label: "Train",
        nav: "Train",
        tagline: "Roleplay & scorecards",
        desc: "Practice as receptionist (dial 9199…). AI plays the customer; review coaching scores.",
        icon: "◐",
        cta: Some("Open training"),
    },
    Skill {
        id: "create",
        href: "/create",
        label: "Create",
        nav: "Create",
        tagline: "Generated content",
        desc: "Follow-up emails, SMS, handoff briefs, and checklists from each call.",
        icon: "✦",
        cta: Some("Open create"),
    },
    Skill {
        id: "brand",
        href: "/brand",
        label: "Brand",
        nav: "Brand",
        tagline: "Voice messages",
        desc: "Create branded announcements — closed periods, after hours — pick a voice or clone one, then download the audio.",
        icon: "◈",
        cta: Some("Open brand"),
    },
];

/// (dial code, name, description, track)
const ROLEPLAY_SCENARIOS: &[(&str, &str, &str, &str)] = &[
    (
        "9199",
        "Already paid — Brett",
        "Brett Anthony · $100 paid today · receipt emailed",
        "collections",
    ),
    (
        "9199promise",
        "Promise to pay — Sochelkara",
        "Tyro / Direo · $150 · payment tomorrow 12pm",
        "collections",
    ),
    (
        "9199gate",
        "Gatekeeper — Gabriel not in",
        "TrueCap · ref 152339 · take a message",
        "collections",
    ),
    (
        "9199vm",
        "Voicemail — leave the script",
        "Beast Fund / TrueCap / Tyro · callback 029 191 4516",
        "collections",
    ),
    (
        "9199hardship",
        "Hardship — liquidation",
        "Ishwar / Toya Lakshmi Pty · Tyro · no capacity",
        "collections",
    ),
    (
        "9199restructure",
        "Restructure — advisor will email",
        "Mark · Beast Fund · timeline this afternoon or tomorrow",
        "collections",
    ),
    (
        "9199later",
        "Callback next week — Sodong",
        "Vance · cannot update now · Tuesday or Wednesday",
        "collections",
    ),
    (
        "9199dead",
        "Dead line — no message",
        "Automated: no message can be left",
        "collections",
    ),
    (
        "9199invoice",
        "Unpaid commercial invoice",
        "Calm owner · ~$6,000 cleaning invoice",
        "enquiry",
    ),
    (
        "9199fees",
        "Fees-focused caller",
        "Joining / monthly / commission questions",
        "enquiry",
    ),
    (
        "9199unsupported",
        "Unsupported debt",
        "Residential rent — LOD template path",
        "enquiry",
    ),
    (
        "9199transfer",
        "Wants a person",
        "Impatient transfer request (no real bridge)",
        "enquiry",
    ),
    (
        "9199angry",
        "Frustrated caller",
        "Called twice already · needs empathy",
        "enquiry",
    ),
];

fn skill(active_id: &str) -> &'static Skill {
    SKILLS
        .iter()
        .find(|s| s.id == active_id)
        .unwrap_or(&SKILLS[0])
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Friendly label for a turn phase, falling back to the session status.
pub fn phase_label(phase: Option<&str>, status: Option<&str>) -> String {
    let raw = phase
        .filter(|p| !p.is_empty())
        .or(status.filter(|s| !s.is_empty()))
        .unwrap_or("");
    let value = raw.trim().to_lowercase();
    match value.as_str() {
        "queued" | "processing" => "Processing…".into(),
        "transcribed" => "Caller heard".into(),
        "reasoned" => "Reply ready".into(),
        "completed" => "Spoken".into(),
        "opening_greeting" => "Greeting".into(),
        "skipped" => "Skipped".into(),
        "failed" => "Failed".into(),
        "" => "Idle".into(),
        other => title(&other.replace('_', " ")),
    }
}

pub fn is_roleplay(session: &LiveCallSession) -> bool {
    session.mode.to_lowercase() == "roleplay"
        || session
            .prompt_profile
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            == "roleplay"
        || session.target_number.as_deref().unwrap_or("") == "9199"
}

fn roleplay_name(session: &LiveCallSession) -> &str {
    or(
        or(&session.roleplay_label, &session.roleplay_scenario),
        "practice",
    )
}

/// "9198 · Enquiries"-style label for the dialled line.
pub fn line_label(
    target: Option<&str>,
    profile: Option<&str>,
    session: Option<&LiveCallSession>,
) -> String {
    let number = target.filter(|t| !t.is_empty()).unwrap_or("—");
    if let Some(s) = session.filter(|s| is_roleplay(s)) {
        return format!("9199 · Roleplay · {}", roleplay_name(s));
    }
    if matches!(profile, Some("sop_enquiries" | "sop_enquiries_realtime")) || number == "9198" {
        return format!("{number} · Enquiries");
    }
    match number {
        "9197" => format!("{number} · AI Pilot"),
        "9199" => "9199 · Roleplay".into(),
        _ => number.into(),
    }
}

fn session_line(s: &LiveCallSession) -> String {
    line_label(
        s.target_number.as_deref(),
        s.prompt_profile.as_deref(),
        Some(s),
    )
}

/// (css class suffix, label) for the status pill.
pub fn session_pill(session: &LiveCallSession) -> (&'static str, &'static str) {
    let raw = session.status.trim().to_lowercase();
    let phase = session.latest_phase.trim().to_lowercase();
    match raw.as_str() {
        "transferred" => return ("transferred", "Transferred"),
        "callback_requested" => return ("callback", "Callback"),
        "completed" => return ("completed", "Ended"),
        "failed" => return ("completed", "Failed"),
        _ => {}
    }
    if matches!(
        phase.as_str(),
        "queued" | "processing" | "transcribed" | "reasoned"
    ) || session.is_active
    {
        return ("live", "Live");
    }
    ("recent", "Recent")
}

/// "Debt: … · Amount: …" chips line; empty when nothing useful is captured.
pub fn format_slots(slots: Option<&Map<String, Value>>) -> String {
    let Some(slots) = slots else {
        return String::new();
    };
    let labels = [
        ("debt_type", "Debt"),
        ("amount", "Amount"),
        ("staff_name", "Staff"),
        ("reference", "Ref"),
    ];
    let mut chips = Vec::new();
    for (key, label) in labels {
        match slots.get(key) {
            Some(value) if !is_blank_fact(value) => {
                chips.push(format!("{label}: {}", py_str(value)))
            }
            _ => {}
        }
    }
    chips.join(" · ")
}

/// Title, body, tips and risk chip for the assist card.
pub fn assist_text(session: &LiveCallSession) -> (String, String, Vec<String>, String) {
    fn out(
        title: &str,
        body: String,
        tips: &[&str],
        risk: &str,
    ) -> (String, String, Vec<String>, String) {
        (
            title.into(),
            body,
            tips.iter().map(|t| (*t).to_string()).collect(),
            risk.into(),
        )
    }
    if is_roleplay(session) {
        let label = roleplay_name(session);
        return out(
            "Roleplay · you are the agent",
            format!("Scenario: {label}. AI is the customer — practice SOP intake."),
            &[
                "Stay in receptionist role",
                "After hangup open Scorecard for coaching",
                "Real transfer is disabled in roleplay",
            ],
            "",
        );
    }
    let intent = session.latest_intent.to_lowercase();
    let slots = &session.slots;
    let action = match session.latest_action.as_deref().filter(|a| !a.is_empty()) {
        Some(a) => a.to_lowercase(),
        None => match slots.get("action").filter(|v| truthy(v)) {
            Some(v) => py_str(v).to_lowercase(),
            None => "continue".into(),
        },
    };
    let debt = slots.get("debt_type").filter(|v| truthy(v));
    let amount = slots.get("amount").filter(|v| truthy(v));
    if action == "transfer" || intent.contains("transfer") {
        return out(
            "Transfer ready",
            "Caller wants a person. Confirm details, then bridge to extension 1000.".into(),
            &[
                "Confirm name / callback number if known",
                "Copy handoff brief before or after transfer",
            ],
            "transfer requested",
        );
    }
    if action == "callback" || intent.contains("callback") {
        return out(
            "Callback",
            "Caller accepted or requested a callback. Capture best time if spoken.".into(),
            &["Note preferred time in CRM", "Confirm phone number"],
            "",
        );
    }
    if action == "decline_debt_type" || intent.contains("unsupported") {
        return out(
            "Unsupported debt type",
            "Point to free Letter of Demand template on slaterbyrne.com.au (Services).".into(),
            &["Do not take residential / childcare / gym / vet / rental as new files"],
            "unsupported debt",
        );
    }
    if intent.contains("fee") {
        return out(
            "Fees",
            "No joining fee · no account maintenance · commission only on successful recovery."
                .into(),
            &[
                "Keep answer short",
                "Offer to continue intake if debt type is supported",
            ],
            "",
        );
    }
    match (debt, amount) {
        (Some(debt), None) => {
            return out(
                "Amount missing",
                format!(
                    "Debt type noted as {}. Still need amount owed.",
                    py_str(debt)
                ),
                &["Ask approximate amount", "Then confirm next steps"],
                "no amount",
            );
        }
        (Some(debt), Some(amount)) => {
            return out(
                "Intake in progress",
                format!(
                    "Debt: {} · Amount: {}. Continue qualification or transfer if requested.",
                    py_str(debt),
                    py_str(amount)
                ),
                &[
                    "Check if debt type is supported",
                    "Offer transfer if they want a person",
                ],
                "",
            );
        }
        _ => {}
    }
    if session.is_active {
        return out(
            "Live enquiry",
            "AI is handling the call. Watch intent and slots update.".into(),
            &["Use Capture / Summary Inform after hangup for full recap"],
            "",
        );
    }
    out(
        "Standby",
        "No active coaching signal for this session.".into(),
        &[],
        "",
    )
}

// ---------------------------------------------------------------------------
// Design system + shell
// ---------------------------------------------------------------------------

fn user_foot_html(ctx: &UiContext) -> String {
    let Some(user) = &ctx.user else {
        return "Slater Byrne Recoveries<br/>Formal operations console · v1".into();
    };
    let name = escape(or(&user.name, "Signed in"));
    let email = escape(&user.email);
    let logout = escape(&ctx.path("/auth/logout"));
    format!(
        "<div class=\"user-chip\"><div class=\"user-name\">{name}</div><div class=\"user-email\">{email}</div>\
         <a class=\"user-logout\" href=\"{logout}\">Sign out</a></div>New Era IT · Voice IN ops"
    )
}

/// The sign-in page. `error` is shown in a red box when non-empty.
pub fn render_login(ctx: &UiContext, error: &str) -> String {
    let login_href = escape(&ctx.path("/auth/login"));
    let err = if error.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="login-error">{}</div>"#, escape(error))
    };
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Sign in · Voice IN</title>
  <style>
{BASE_CSS}
{LOGIN_CSS}  </style>
</head>
<body>
  <div class="login-wrap">
    <div class="login-card">
      <div class="brand-glyph" style="width:42px;height:42px">VI</div>
      <h1>Voice IN ops</h1>
      <p>Sign in with your New Era IT Microsoft account to open the operations portal.</p>
      {err}
      <a class="btn ms-btn" href="{login_href}">
        <svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 23 23" aria-hidden="true">
          <rect width="10" height="10" fill="#f25022"/>
          <rect x="12" width="10" height="10" fill="#7fba00"/>
          <rect y="12" width="10" height="10" fill="#00a4ef"/>
          <rect x="12" y="12" width="10" height="10" fill="#ffb900"/>
        </svg>
        Sign in with Microsoft
      </a>
      <p class="login-note">Access is limited to the Voice IN Ops group in Entra ID.</p>
    </div>
  </div>
</body>
</html>"##
    )
}

fn nav_html(ctx: &UiContext, active_id: &str) -> String {
    SKILLS
        .iter()
        .map(|s| {
            let cls = if s.id == active_id {
                "nav-item active"
            } else {
                "nav-item"
            };
            format!(
                r#"<a class="{cls}" href="{}"><span class="nav-icon">{}</span><span>{}</span></a>"#,
                escape(&ctx.path(s.href)),
                escape(s.icon),
                escape(s.nav)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Default)]
struct Shell<'a> {
    active_id: &'a str,
    title: &'a str,
    kicker: &'a str,
    /// Inserted as-is (callers escape).
    subtitle: &'a str,
    body: &'a str,
    actions_html: &'a str,
    extra_css: &'a str,
    extra_js: &'a str,
    page_title: Option<&'a str>,
}

fn shell(ctx: &UiContext, page: Shell<'_>) -> String {
    let pt = match page.page_title.filter(|t| !t.is_empty()) {
        Some(t) => escape(t),
        None => escape(&format!("{} · Voice IN", skill(page.active_id).label)),
    };
    let nav = nav_html(ctx, page.active_id);
    let foot = user_foot_html(ctx);
    let kicker = escape(page.kicker);
    let title = escape(page.title);
    let Shell {
        subtitle,
        body,
        actions_html,
        extra_css,
        extra_js,
        ..
    } = page;
    let script = if extra_js.is_empty() {
        String::new()
    } else {
        format!("<script>{extra_js}</script>")
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{pt}</title>
  <style>
{BASE_CSS}
{extra_css}
  </style>
</head>
<body>
  <div class="app-shell">
    <aside class="sidebar">
      <div class="brand">
        <div class="brand-mark">
          <div class="brand-glyph">VI</div>
          <div>
            <h1>Voice IN</h1>
          </div>
        </div>
        <p>Enquiries platform</p>
      </div>
      <div class="nav-section-label">Workspace</div>
      <nav class="nav-list" aria-label="Main">
        {nav}
      </nav>
      <div class="sidebar-foot">
        {foot}
      </div>
    </aside>
    <div class="main">
      <header class="topbar">
        <div>
          <div class="topbar-kicker">{kicker}</div>
          <h2>{title}</h2>
          <p class="sub">{subtitle}</p>
        </div>
        <div class="topbar-actions">{actions_html}</div>
      </header>
      <div class="content">
        {body}
      </div>
    </div>
  </div>
  {script}
</body>
</html>"#
    )
}

// ---------------------------------------------------------------------------
// Landing (menu)
// ---------------------------------------------------------------------------

pub fn render_home(ctx: &UiContext, sessions: &[LiveCallSession]) -> String {
    let active_n = sessions.iter().filter(|s| s.is_active).count();
    let recent_n = sessions.len() - active_n;
    let rec_n = sessions.iter().filter(|s| s.has_recording).count();
    let roleplay_n = sessions.iter().filter(|s| is_roleplay(s)).count();

    let mut cards = String::new();
    for s in SKILLS.iter().filter(|s| s.id != "home") {
        let href = escape(&ctx.path(s.href));
        let icon = escape(s.icon);
        let tagline = escape(s.tagline);
        let label = escape(s.label);
        let desc = escape(s.desc);
        let cta = escape(s.cta.unwrap_or("Open"));
        cards.push_str(&format!(
            r#"
            <a class="skill-card" href="{href}">
              <div class="icon">{icon}</div>
              <div class="tagline">{tagline}</div>
              <h3>{label}</h3>
              <p>{desc}</p>
              <div class="cta">{cta} →</div>
            </a>
            "#
        ));
    }

    let body = format!(
        r#"
    <section class="hero-panel">
      <div class="eyebrow">Command centre</div>
      <h2>Select a capability</h2>
      <p>
        Each skill opens in its own formal workspace. Use the menu at any time to move between
        Live Console, Assist, Record, Capture, Train, Create, and Brand.
      </p>
      <div class="hero-meta">
        <span><strong>{active_n}</strong> live</span>
        <span><strong>{recent_n}</strong> recent</span>
        <span><strong>{rec_n}</strong> with recording</span>
        <span><strong>{roleplay_n}</strong> roleplay</span>
      </div>
    </section>

    <div class="stats-row">
      <div class="stat"><div class="n">{active_n}</div><div class="l">Live calls</div></div>
      <div class="stat"><div class="n">{recent_n}</div><div class="l">Recent sessions</div></div>
      <div class="stat"><div class="n">9198</div><div class="l">AI enquiries</div></div>
      <div class="stat"><div class="n">9199</div><div class="l">Roleplay train</div></div>
    </div>

    <div class="section-label">Capabilities</div>
    <div class="skill-grid">
      {cards}
    </div>
    "#
    );
    shell(
        ctx,
        Shell {
            active_id: "home",
            title: "Home",
            kicker: "Voice IN · Overview",
            subtitle: "A formal menu of skills — open only what you need.",
            body: &body,
            actions_html: &format!(
                r#"<a class="btn btn-primary" href="{}">Open Live Console</a>"#,
                ctx.path("/console")
            ),
            page_title: Some("Voice IN · Home"),
            ..Default::default()
        },
    )
}

// ---------------------------------------------------------------------------
// Session list helpers for skill pages
// ---------------------------------------------------------------------------

fn session_row_actions(ctx: &UiContext, session: &LiveCallSession) -> String {
    let sid = escape(&session.session_id);
    let summary = ctx.path("/summary/");
    let first = if is_roleplay(session) {
        "Scorecard"
    } else {
        "Summary"
    };
    let mut bits = format!(r#"<a class="btn secondary" href="{summary}{sid}">{first}</a>"#);
    if session.has_recording {
        let audio = ctx.path(&format!("/api/telephony/sessions/{sid}/recording"));
        bits.push_str(&format!(
            r#"<a class="btn secondary" href="{audio}">Audio</a>"#
        ));
    }
    bits
}

fn sessions_table(ctx: &UiContext, sessions: &[LiveCallSession], empty: &str) -> String {
    if sessions.is_empty() {
        return format!(r#"<div class="empty-box">{}</div>"#, escape(empty));
    }
    let mut rows = String::new();
    for s in sessions {
        let (pill_c, pill_l) = session_pill(s);
        let pill_l = escape(pill_l);
        let line = escape(&session_line(s));
        let intent = escape(&or(&s.latest_intent, "—").replace('_', " "));
        let short_id = escape(take_chars(&s.session_id, 18));
        let turns = s.turn_count;
        let rec = if s.has_recording { "Yes" } else { "—" };
        let actions = session_row_actions(ctx, s);
        rows.push_str(&format!(
            r#"
            <tr>
              <td>
                <div style="font-weight:600">{line}</div>
                <div class="meta" style="margin:2px 0 0">{short_id}…</div>
              </td>
              <td><span class="pill pill-{pill_c}">{pill_l}</span></td>
              <td>{intent}</td>
              <td>{turns}</td>
              <td>{rec}</td>
              <td><div class="row-actions">{actions}</div></td>
            </tr>
            "#
        ));
    }
    format!(
        r#"
    <div class="panel table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th>Line / session</th>
            <th>Status</th>
            <th>Intent</th>
            <th>Turns</th>
            <th>Rec</th>
            <th>Actions</th>
          </tr>
        </thead>
        <tbody>{rows}</tbody>
      </table>
    </div>
    "#
    )
}

// ---------------------------------------------------------------------------
// Live console (interactive)
// ---------------------------------------------------------------------------

pub fn render_console(ctx: &UiContext, sessions: &[LiveCallSession]) -> String {
    let card = |s: &LiveCallSession| console_session_card_ssr(ctx, s);
    let active: Vec<String> = sessions.iter().filter(|s| s.is_active).map(card).collect();
    let recent: Vec<String> = sessions
        .iter()
        .filter(|s| !s.is_active)
        .take(6)
        .map(card)
        .collect();
    let active_cards = if active.is_empty() {
        EMPTY_LIVE.to_string()
    } else {
        active.join("\n")
    };
    let recent_cards = if recent.is_empty() {
        r#"<article class="call-card empty-state"><p class="muted">No recent calls yet.</p></article>"#.to_string()
    } else {
        recent.join("\n")
    };

    let body = format!(
        r#"
    <div id="telephony-status" class="statusline">Connecting to live feed…</div>
    <div class="section-label"><span class="live-dot">Live now</span></div>
    <div id="active-board" class="cards">{active_cards}</div>
    <div class="section-label">Recent</div>
    <div id="recent-board" class="cards">{recent_cards}</div>
    <details class="drawer">
      <summary>Device upload (lab)</summary>
      <div class="drawer-body">
        <p class="muted" style="margin-top:12px">Optional WAV upload for offline device testing.</p>
        <form id="upload-form">
          <input id="audio-file" type="file" accept="audio/wav" required />
          <button class="btn btn-primary" type="submit">Process upload</button>
        </form>
        <pre id="upload-result">Awaiting upload…</pre>
      </div>
    </details>
    "#
    );
    shell(
        ctx,
        Shell {
            active_id: "console",
            title: "Live Console",
            kicker: "Operations · Real-time",
            subtitle: "Caller speech, AI thinking, and replies — updated live.",
            body: &body,
            actions_html: &format!(
                r#"<a class="btn btn-ghost" href="{}">← Menu</a><button id="refresh-sessions" type="button" class="btn btn-ghost">Refresh</button>"#,
                ctx.path("/")
            ),
            extra_js: &console_js(ctx),
            page_title: Some("Live Console · Voice IN"),
            ..Default::default()
        },
    )
}

const EMPTY_LIVE: &str = r#"
    <article class="call-card empty-state" data-empty="1">
      <h3 style="margin:0 0 8px;color:var(--ink)">No live call right now</h3>
      <p class="muted">Dial <code>9198</code> (AI enquiries) or <code>9199</code> (roleplay) from MicroSIP.</p>
    </article>
    "#;

/// Server-side card; the live JS rebuilds cards after first paint.
fn console_session_card_ssr(ctx: &UiContext, session: &LiveCallSession) -> String {
    let (pill_c, pill_l) = session_pill(session);
    let pill_l = escape(pill_l);
    let line = escape(&session_line(session));
    let phase = escape(&phase_label(
        Some(&session.latest_phase),
        Some(&session.status),
    ));
    let slots = format_slots(Some(&session.slots));
    let roleplay = is_roleplay(session);
    let human_eyebrow = if roleplay {
        "You just said"
    } else {
        "Caller just said"
    };
    let ai_eyebrow = if roleplay {
        "Customer (AI) said"
    } else {
        "AI reply"
    };
    let (title, body, tips, risk) = assist_text(session);
    let tips_html: String = tips
        .iter()
        .map(|t| format!("<li>{}</li>", escape(t)))
        .collect();
    let risk_html = if risk.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="chip risk">{}</span>"#, escape(&risk))
    };
    let assist_eyebrow = if roleplay {
        "Train · Roleplay"
    } else {
        "Ride-along · Assist"
    };
    let title = escape(&title);
    let body = escape(&body);
    let assist = format!(
        r#"
      <div class="assist-card">
        <div style="display:flex;justify-content:space-between;gap:8px;margin-bottom:4px">
          <div class="eyebrow">{assist_eyebrow}</div>
          {risk_html}
        </div>
        <div class="assist-title">{title}</div>
        <p class="assist-body">{body}</p>
        <ul class="assist-tips">{tips_html}</ul>
      </div>
    "#
    );
    let sid = escape(&session.session_id);
    let is_active = session.is_active;
    let turns = session.turn_count;
    let transcript = escape(or(&session.latest_transcript, "—"));
    let response = escape(or(&session.latest_response, "—"));
    let slot_row = if slots.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="slot-row">{}</div>"#, escape(&slots))
    };
    let summary = ctx.path("/summary/");
    let summary_label = if roleplay {
        "Scorecard"
    } else {
        "Summary Inform"
    };
    let copy_label = if roleplay {
        "Copy coach notes"
    } else {
        "Copy handoff brief"
    };
    format!(
        r#"
    <article class="call-card" data-session-id="{sid}" data-active="{is_active}">
      <div class="call-top">
        <div>
          <div class="line-title">{line}</div>
          <div class="phase-line">{phase} · turns {turns}</div>
        </div>
        <span class="pill pill-{pill_c}">{pill_l}</span>
      </div>
      <div class="now-grid">
        <div class="now-block">
          <div class="eyebrow">{human_eyebrow}</div>
          <p>{transcript}</p>
        </div>
        <div class="now-block accent">
          <div class="eyebrow">{ai_eyebrow}</div>
          <p>{response}</p>
        </div>
      </div>
      {slot_row}
      {assist}
      <div class="card-actions">
        <a class="btn summary-btn" href="{summary}{sid}">{summary_label}</a>
        <button type="button" class="secondary copy-handoff" data-session-id="{sid}">{copy_label}</button>
      </div>
    </article>
    "#
    )
}

fn console_js(ctx: &UiContext) -> String {
    let base_json = json_str(ctx.base());
    format!(
        "\n    const BASE_PATH = {base_json};\n    function P(u) {{ return BASE_PATH + u; }}\n    {CONSOLE_JS}"
    )
}

// ---------------------------------------------------------------------------
// Skill pages
// ---------------------------------------------------------------------------

pub fn render_assist(ctx: &UiContext, sessions: &[LiveCallSession]) -> String {
    let active: Vec<&LiveCallSession> = sessions.iter().filter(|s| s.is_active).collect();
    let shown: Vec<&LiveCallSession> = if active.is_empty() {
        sessions.iter().take(8).collect()
    } else {
        active
    };
    let mut cards = String::new();
    for s in shown {
        let (title, body, tips, risk) = assist_text(s);
        let (pill_c, pill_l) = session_pill(s);
        let line = session_line(s);
        let mut tips_html: String = tips
            .iter()
            .map(|t| format!("<li>{}</li>", escape(t)))
            .collect();
        if tips_html.is_empty() {
            tips_html = "<li class='muted'>-</li>".into();
        }
        let risk_html = if risk.is_empty() {
            String::new()
        } else {
            format!(r#"<span class="chip risk">{}</span>"#, escape(&risk))
        };
        let intent_label = or(&s.latest_intent, "-").replace('_', " ");
        let card = [
            r#"<article class="panel" style="margin-bottom:12px">"#.to_string(),
            r#"<div style="display:flex;justify-content:space-between;gap:12px;flex-wrap:wrap;margin-bottom:10px">"#.to_string(),
            "<div>".to_string(),
            format!(r#"<div class="line-title">{}</div>"#, escape(&line)),
            format!(r#"<div class="meta">Intent: {} · turns {}</div>"#, escape(&intent_label), s.turn_count),
            "</div>".to_string(),
            format!(r#"<span class="pill pill-{}">{}</span>"#, pill_c, escape(pill_l)),
            "</div>".to_string(),
            r#"<div class="assist-card" style="margin:0">"#.to_string(),
            r#"<div style="display:flex;justify-content:space-between;align-items:center;gap:8px">"#.to_string(),
            r#"<div class="eyebrow">Coaching</div>"#.to_string(),
            risk_html,
            "</div>".to_string(),
            format!(r#"<div class="assist-title">{}</div>"#, escape(&title)),
            format!(r#"<p class="assist-body">{}</p>"#, escape(&body)),
            format!(r#"<ul class="assist-tips">{tips_html}</ul>"#),
            "</div>".to_string(),
            r#"<div class="card-actions">"#.to_string(),
            format!(
                r#"<a class="btn summary-btn" href="{}{}">Open summary</a>"#,
                ctx.path("/summary/"),
                escape(&s.session_id)
            ),
            format!(r#"<a class="btn secondary" href="{}">Watch live</a>"#, ctx.path("/console")),
            "</div></article>".to_string(),
        ];
        cards.push_str(&card.join("\n"));
    }
    let mut body = String::from(
        r#"<p class="statusline">Ride-along coaching for operators — focused on the current conversation state.</p>"#,
    );
    if cards.is_empty() {
        body.push_str(r#"<div class="empty-box">No sessions yet. Take a call on <code>9198</code> or open Live Console.</div>"#);
    } else {
        body.push_str(&cards);
    }
    shell(
        ctx,
        Shell {
            active_id: "assist",
            title: "Assist",
            kicker: "Skill · Ride-along",
            subtitle: "Coaching cards and next-best guidance while enquiries are handled.",
            body: &body,
            actions_html: &menu_and(ctx, "/console", "Live Console"),
            ..Default::default()
        },
    )
}

fn menu_link(ctx: &UiContext) -> String {
    format!(
        r#"<a class="btn btn-ghost" href="{}">← Menu</a>"#,
        ctx.path("/")
    )
}

fn menu_and(ctx: &UiContext, href: &str, label: &str) -> String {
    format!(
        r#"{}<a class="btn btn-primary" href="{}">{label}</a>"#,
        menu_link(ctx),
        ctx.path(href)
    )
}

pub fn render_record(ctx: &UiContext, sessions: &[LiveCallSession]) -> String {
    let with_rec: Vec<&LiveCallSession> = sessions.iter().filter(|s| s.has_recording).collect();
    let mut blocks = String::new();
    let rec_base = ctx.path("/api/telephony/sessions/");
    let summary = ctx.path("/summary/");
    for s in with_rec.iter().take(40) {
        let (pill_c, pill_l) = session_pill(s);
        let pill_l = escape(pill_l);
        let line = escape(&session_line(s));
        let sid = escape(&s.session_id);
        let when = escape(or(&s.updated_at, &s.started_at));
        blocks.push_str(&format!(
            r#"
            <article class="panel" style="margin-bottom:12px">
              <div style="display:flex;justify-content:space-between;gap:12px;flex-wrap:wrap;margin-bottom:10px">
                <div>
                  <div class="line-title">{line}</div>
                  <div class="meta">{sid} · {when}</div>
                </div>
                <span class="pill pill-{pill_c}">{pill_l}</span>
              </div>
              <audio controls preload="metadata" style="width:100%">
                <source src="{rec_base}{sid}/recording" type="audio/wav" />
              </audio>
              <div class="card-actions">
                <a class="btn secondary" href="{summary}{sid}">Open summary</a>
              </div>
            </article>
            "#
        ));
    }
    if blocks.is_empty() {
        blocks.push_str(r#"<div class="empty-box">No recordings found yet. Complete an AI call to populate the library.</div>"#);
    }
    let rec_count = with_rec.len();
    let total = sessions.len();
    let table = sessions_table(ctx, &sessions[..sessions.len().min(30)], "No sessions.");
    let body = format!(
        r#"
    <p class="statusline">Mixed call recordings (caller + AI). New 9198 / 2900 / 9199 calls are captured automatically.</p>
    <div class="stats-row">
      <div class="stat"><div class="n">{rec_count}</div><div class="l">Recordings available</div></div>
      <div class="stat"><div class="n">{total}</div><div class="l">Sessions scanned</div></div>
    </div>
    {blocks}
    {table}
    "#
    );
    shell(
        ctx,
        Shell {
            active_id: "record",
            title: "Record",
            kicker: "Skill · Call library",
            subtitle: "Playback and archive of conversation audio.",
            body: &body,
            actions_html: &menu_link(ctx),
            ..Default::default()
        },
    )
}

pub fn render_capture(ctx: &UiContext, sessions: &[LiveCallSession]) -> String {
    let analysed: Vec<&LiveCallSession> = sessions
        .iter()
        .filter(|s| s.has_post_analysis || s.post_analysis.is_some())
        .collect();
    let shown: Vec<&LiveCallSession> = if analysed.is_empty() {
        sessions.iter().collect()
    } else {
        analysed
    };
    let summary_href = ctx.path("/summary/");
    let mut cards = String::new();
    for s in shown.into_iter().take(15) {
        let a = s.post_analysis.as_ref();
        let (pill_c, pill_l) = session_pill(s);
        let pill_l = escape(pill_l);
        let line = escape(&session_line(s));
        let summary = or(
            or(a.map_or("", |a| &a.summary), &s.latest_transcript),
            "Open for full analysis.",
        );
        let intent = or(
            a.map_or(s.latest_intent.as_str(), |a| &a.primary_intent),
            "—",
        );
        let outcome = or(a.map_or(s.status.as_str(), |a| &a.outcome), "—");
        let intent = escape(&intent.replace('_', " "));
        let outcome = escape(&outcome.replace('_', " "));
        let short = escape(take_chars(summary, 400));
        let more = if summary.chars().count() > 400 {
            "…"
        } else {
            ""
        };
        let turns = s.turn_count;
        let sid = escape(&s.session_id);
        cards.push_str(&format!(
            r#"
            <article class="panel" style="margin-bottom:12px">
              <div style="display:flex;justify-content:space-between;gap:12px;flex-wrap:wrap">
                <div>
                  <div class="eyebrow">Primary intent</div>
                  <div class="intent-hero" style="font-size:1.1rem">{intent}</div>
                  <div class="line-title" style="font-weight:500;color:var(--muted);margin-bottom:8px">{line}</div>
                </div>
                <span class="pill pill-{pill_c}">{pill_l}</span>
              </div>
              <p class="summary-text" style="font-size:0.95rem">{short}{more}</p>
              <div class="meta" style="margin-top:10px">Outcome: {outcome} · turns {turns}</div>
              <div class="card-actions">
                <a class="btn summary-btn" href="{summary_href}{sid}">Open Summary Inform</a>
              </div>
            </article>
            "#
        ));
    }
    if cards.is_empty() {
        cards.push_str(r#"<div class="empty-box">No captured analyses yet. Finish a call, then open Summary Inform.</div>"#);
    }
    let body = format!(
        r#"
    <p class="statusline">Summary Inform — recover intent, facts, risks, and handoff from completed conversations.</p>
    {cards}
    "#
    );
    shell(
        ctx,
        Shell {
            active_id: "capture",
            title: "Capture",
            kicker: "Skill · Summary Inform",
            subtitle: "Structured post-call recovery of what was said and what matters next.",
            body: &body,
            actions_html: &menu_and(ctx, "/console", "Live Console"),
            ..Default::default()
        },
    )
}

/// `str(obj.get(key) or "")`-style lookup: the value as text when it is truthy.
fn text_of(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).filter(|v| truthy(v)).map(py_str)
}

/// (dial code, name, description, track) cards from the client pack, else the built-in list.
fn train_scenario_cards(rp: Option<&Map<String, Value>>) -> Vec<(String, String, String, String)> {
    let scenarios = rp
        .and_then(|rp| rp.get("scenarios"))
        .and_then(Value::as_object)
        .filter(|s| !s.is_empty());
    if let (Some(rp), Some(scenarios)) = (rp, scenarios) {
        let default = text_of(rp, "default_scenario").unwrap_or_else(|| "pay".into());
        return scenarios
            .iter()
            .filter_map(|(key, spec)| {
                let spec = spec.as_object()?;
                let code = if *key == default {
                    "9199".to_string()
                } else {
                    format!("9199{key}")
                };
                Some((
                    code,
                    text_of(spec, "label").unwrap_or_else(|| key.clone()),
                    text_of(spec, "blurb")
                        .or_else(|| text_of(spec, "opening"))
                        .unwrap_or_default(),
                    text_of(spec, "track").unwrap_or_else(|| "enquiry".into()),
                ))
            })
            .collect();
    }
    ROLEPLAY_SCENARIOS
        .iter()
        .map(|(code, name, desc, track)| {
            (
                code.to_string(),
                name.to_string(),
                desc.to_string(),
                track.to_string(),
            )
        })
        .collect()
}

fn scenario_grid<'a>(cards: impl Iterator<Item = &'a (String, String, String, String)>) -> String {
    cards
        .map(|(code, name, desc, _track)| {
            let (code, name, desc) = (escape(code), escape(name), escape(desc));
            format!(
                r#"
        <div class="scenario-card">
          <div class="scenario-code">{code}</div>
          <div class="scenario-name">{name}</div>
          <p>{desc}</p>
        </div>
        "#
            )
        })
        .collect()
}

fn render_train_tree(tree: Option<&Map<String, Value>>) -> String {
    let Some(tree) = tree.filter(|t| !t.is_empty()) else {
        return String::new();
    };
    let mut items = String::new();
    for branch in tree
        .get("branches")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(branch) = branch.as_object() else {
            continue;
        };
        let dial_html = match text_of(branch, "dial") {
            Some(dial) => format!(
                r#"<div class="tree-dial">Practice: <code>{}</code></div>"#,
                escape(&dial)
            ),
            None => String::new(),
        };
        let when = escape(&text_of(branch, "when").unwrap_or_default());
        let action = escape(&text_of(branch, "do").unwrap_or_default());
        items.push_str(&format!(
            r#"
            <li>
              <div class="tree-when">{when}</div>
              <div class="tree-do">{action}</div>
              {dial_html}
            </li>
            "#
        ));
    }
    let mut meta = String::new();
    for (key, label) in [
        ("identify", "Open"),
        ("purpose", "Purpose"),
        ("callback", "Callback"),
    ] {
        if let Some(text) = text_of(tree, key) {
            meta.push_str(&format!(
                r#"<div class="tree-identify"><strong>{label}:</strong> {}</div>"#,
                escape(&text)
            ));
        }
    }
    let subtitle = escape(&text_of(tree, "subtitle").unwrap_or_else(|| "Initial tree".into()));
    let title = escape(&text_of(tree, "title").unwrap_or_else(|| "Call script".into()));
    format!(
        r#"
    <section class="hero-panel train-tree">
      <div class="eyebrow">{subtitle}</div>
      <h2 style="font-size:1.25rem">{title}</h2>
      {meta}
      <ol class="tree-list">{items}</ol>
    </section>
    "#
    )
}

/// The Train page. `client_pack` is the client pack JSON (`Value::Null` when there is none);
/// its `roleplay` section supplies the scenarios and call-script tree.
pub fn render_train(ctx: &UiContext, sessions: &[LiveCallSession], client_pack: &Value) -> String {
    let rp = client_pack.get("roleplay").and_then(Value::as_object);
    let cards = train_scenario_cards(rp);
    let tree = rp.and_then(|rp| rp.get("tree")).and_then(Value::as_object);
    let tree_html = render_train_tree(tree);
    let role = |key: &str, default: &str| {
        escape(
            &tree
                .and_then(|t| text_of(t, key))
                .unwrap_or_else(|| default.into()),
        )
    };
    let staff_role = role("staff_role", "You are the SBR collector");
    let ai_role = role("ai_role", "AI is the debtor, gatekeeper, or voicemail");
    let summary = ctx.path("/summary/");
    let mut score_rows = String::new();
    for s in sessions.iter().filter(|s| is_roleplay(s)).take(12) {
        let sc = s
            .post_analysis
            .as_ref()
            .and_then(|a| a.roleplay_scorecard.as_ref());
        let (pill_c, pill_l) = session_pill(s);
        let pill_l = escape(pill_l);
        let score_bit = match sc {
            Some(sc) => format!(
                "<strong>{}</strong> · Grade {}",
                sc.overall_score,
                escape(&sc.grade)
            ),
            None => "Score pending".into(),
        };
        let name = escape(roleplay_name(s));
        let turns = s.turn_count;
        let sid = escape(&s.session_id);
        score_rows.push_str(&format!(
            r#"
            <tr>
              <td>{name}</td>
              <td><span class="pill pill-{pill_c}">{pill_l}</span></td>
              <td>{score_bit}</td>
              <td>{turns}</td>
              <td><a class="btn secondary" href="{summary}{sid}">Scorecard</a></td>
            </tr>
            "#
        ));
    }
    let table = if score_rows.is_empty() {
        r#"<div class="empty-box" style="margin-top:16px">No roleplay sessions yet. Dial a scenario from MicroSIP.</div>"#.to_string()
    } else {
        format!(
            r#"
        <div class="panel table-wrap" style="margin-top:16px">
          <table class="data">
            <thead><tr><th>Scenario</th><th>Status</th><th>Score</th><th>Turns</th><th></th></tr></thead>
            <tbody>{score_rows}</tbody>
          </table>
        </div>
        "#
        )
    };
    let block = |label: &str, grid: String| {
        if grid.is_empty() {
            String::new()
        } else {
            format!(
                r#"<div class="section-label">{label}</div><div class="scenario-grid">{grid}</div>"#
            )
        }
    };
    let collections_block = block(
        "Collections scenarios (from live recordings)",
        scenario_grid(cards.iter().filter(|c| c.3 == "collections")),
    );
    let enquiry_block = block(
        "Enquiry receptionist scenarios",
        scenario_grid(cards.iter().filter(|c| c.3 != "collections")),
    );
    let body = format!(
        r#"
    <section class="hero-panel" style="margin-bottom:20px">
      <div class="eyebrow">How to practice</div>
      <h2 style="font-size:1.25rem">{staff_role} · {ai_role}</h2>
      <p>From MicroSIP (ext 1000), dial a scenario code. After hangup, open the scorecard for coaching.</p>
    </section>
    {tree_html}
    {collections_block}
    {enquiry_block}
    <div class="section-label">Recent practice</div>
    {table}
    "#
    );
    shell(
        ctx,
        Shell {
            active_id: "train",
            title: "Train",
            kicker: "Skill · Roleplay",
            subtitle:
                "Practice the collections script from live SBR recordings, then review scorecards.",
            body: &body,
            actions_html: &menu_and(ctx, "/console", "Watch live"),
            ..Default::default()
        },
    )
}

pub fn render_create(ctx: &UiContext, sessions: &[LiveCallSession]) -> String {
    let packs: Vec<&LiveCallSession> = sessions
        .iter()
        .filter(|s| s.content.as_ref().is_some_and(|c| !c.artifacts.is_empty()))
        .collect();
    let summary = ctx.path("/summary/");
    let mut blocks = String::new();
    for s in packs.iter().take(12) {
        let line = escape(&session_line(s));
        let mut arts = String::new();
        for a in s.content.iter().flat_map(|c| c.artifacts.iter()).take(4) {
            let title = escape(&a.title);
            let full = escape(&a.body);
            let short = escape(take_chars(&a.body, 1200));
            let more = if a.body.chars().count() > 1200 {
                "…"
            } else {
                ""
            };
            arts.push_str(&format!(
                r#"
            <div class="content-item">
              <div class="content-item-top">
                <div class="eyebrow">{title}</div>
                <button type="button" class="secondary copy-content-btn" data-copy-body="{full}">Copy</button>
              </div>
              <pre class="draft">{short}{more}</pre>
            </div>
            "#
            ));
        }
        let sid = escape(&s.session_id);
        blocks.push_str(&format!(
            r#"
            <article class="panel" style="margin-bottom:14px">
              <div class="line-title" style="margin-bottom:4px">{line}</div>
              <div class="meta" style="margin-bottom:10px">{sid}</div>
              {arts}
              <div class="card-actions">
                <a class="btn secondary" href="{summary}{sid}">Full pack in summary</a>
              </div>
            </article>
            "#
        ));
    }
    if blocks.is_empty() {
        blocks.push_str(
            r#"<div class="empty-box">No content packs yet. Open Summary Inform after a call to generate Create artefacts.</div>"#,
        );
    }
    let count = packs.len();
    let body = format!(
        r#"
    <p class="statusline">Generated artefacts from conversations — email, SMS, handoff, checklists.</p>
    <div class="stats-row">
      <div class="stat"><div class="n">{count}</div><div class="l">Calls with content</div></div>
    </div>
    {blocks}
    <script>{COPY_CREATE_JS}</script>
    "#
    );
    shell(
        ctx,
        Shell {
            active_id: "create",
            title: "Create",
            kicker: "Skill · Content pack",
            subtitle: "Operator-ready drafts produced from each conversation.",
            body: &body,
            actions_html: &menu_and(ctx, "/capture", "Open Capture"),
            ..Default::default()
        },
    )
}

// ---------------------------------------------------------------------------
// Summary / scorecard (shared shell)
// ---------------------------------------------------------------------------

/// The Summary Inform page (a scorecard for roleplay calls). `analysis` is the
/// post-call analysis to show, which the caller generates or loads first.
pub fn render_summary(
    ctx: &UiContext,
    session: &LiveCallSession,
    analysis: &CallPostAnalysis,
) -> String {
    let roleplay = is_roleplay(session);
    let line = session_line(session);
    let (pill_c, pill_l) = session_pill(session);
    let pill_l = escape(pill_l);

    // Slots first, then key facts on top (a fact replaces a slot of the same name in place).
    let mut merged = if analysis.recovered_slots.is_empty() {
        session.slots.clone()
    } else {
        analysis.recovered_slots.clone()
    };
    for (key, value) in &analysis.key_facts {
        merged.insert(key.clone(), value.clone());
    }
    let fact_rows: String = merged
        .iter()
        .filter(|(_, value)| !is_blank_fact(value))
        .map(|(key, value)| {
            format!(
                "<tr><th>{}</th><td>{}</td></tr>",
                escape(&title(&key.replace('_', " "))),
                escape(&py_str(value))
            )
        })
        .collect();
    let facts_table = if fact_rows.is_empty() {
        r#"<p class="muted">No structured facts recovered.</p>"#.to_string()
    } else {
        format!(r#"<table class="facts-table"><tbody>{fact_rows}</tbody></table>"#)
    };
    let risks_html = if analysis.risks.is_empty() {
        r#"<p class="muted">No risks flagged.</p>"#.to_string()
    } else {
        analysis
            .risks
            .iter()
            .map(|r| {
                format!(
                    r#"<span class="chip risk">{}</span>"#,
                    escape(&r.replace('_', " "))
                )
            })
            .collect()
    };
    let actions_html = if analysis.action_items.is_empty() {
        r#"<p class="muted">No action items.</p>"#.to_string()
    } else {
        let rows: String = analysis
            .action_items
            .iter()
            .map(|a| {
                let due = match a.due.as_deref().filter(|d| !d.is_empty()) {
                    Some(due) => format!(" · {}", escape(due)),
                    None => String::new(),
                };
                format!(
                    "<li><strong>{}</strong> — {}{due}</li>",
                    escape(&a.owner),
                    escape(&a.task)
                )
            })
            .collect();
        format!("<ul class='action-list'>{rows}</ul>")
    };
    let next_action = escape(
        &or(
            or(
                &analysis.next_best_action,
                session.latest_action.as_deref().unwrap_or(""),
            ),
            "—",
        )
        .replace('_', " "),
    );
    let email = escape(&analysis.follow_up_email_draft);
    let email_shown = if email.is_empty() { "—" } else { &email };
    let handoff = escape(or(&session.handoff_brief, &analysis.internal_handoff_brief).trim());
    let handoff_shown = if handoff.is_empty() { "—" } else { &handoff };

    let mut content_blocks = String::new();
    for art in session.content.iter().flat_map(|c| c.artifacts.iter()) {
        let (key, art_title, body) = (escape(&art.key), escape(&art.title), escape(&art.body));
        content_blocks.push_str(&format!(
            r#"
                <div class="content-item" data-copy-key="{key}">
                  <div class="content-item-top">
                    <div class="eyebrow">{art_title}</div>
                    <button type="button" class="secondary copy-content-btn" data-copy-body="{body}">Copy</button>
                  </div>
                  <pre class="draft">{body}</pre>
                </div>
                "#
        ));
    }
    let content_html = if content_blocks.is_empty() {
        String::new()
    } else {
        format!(
            r#"
        <section class="panel" style="margin-bottom:14px">
          <div class="eyebrow">Generated content · Create</div>
          <p class="muted" style="margin-bottom:10px">Artefacts from this conversation.</p>
          {content_blocks}
        </section>
        "#
        )
    };
    let sid = escape(&session.session_id);
    let recording_html = if session.has_recording {
        let rec_base = ctx.path("/api/telephony/sessions/");
        format!(
            r#"
        <section class="panel" style="margin-bottom:14px">
          <div class="eyebrow">Call recording</div>
          <audio controls preload="metadata" style="width:100%;margin-top:8px"
            src="{rec_base}{sid}/recording"></audio>
        </section>
        "#
        )
    } else {
        r#"
        <section class="panel" style="margin-bottom:14px">
          <div class="eyebrow">Call recording</div>
          <p class="muted">No recording file found for this call yet.</p>
        </section>
        "#
        .to_string()
    };

    let human_name = if roleplay { "Staff" } else { "Caller" };
    let ai_name = if roleplay { "AI customer" } else { "AI" };
    let mut turn_rows: Vec<String> = Vec::new();
    for turn in &session.turns {
        let is_opening =
            matches!(
                turn.intent.as_str(),
                "opening_greeting" | "roleplay_customer_opening"
            ) || (turn.turn == Some(0) && !turn.response.is_empty() && turn.transcript.is_empty());
        if is_opening {
            let open_label = if roleplay {
                "AI customer opening"
            } else {
                "AI greeting"
            };
            turn_rows.push(format!(
                r#"<div class="sum-turn"><div class="eyebrow">{open_label}</div><p>{}</p></div>"#,
                escape(or(&turn.response, "—"))
            ));
            continue;
        }
        let mut bits = String::new();
        if !turn.transcript.is_empty() {
            bits.push_str(&format!(
                r#"<div class="sum-line caller"><strong>{human_name}</strong> {}</div>"#,
                escape(&turn.transcript)
            ));
        }
        if !turn.response.is_empty() {
            let intent = if turn.intent.is_empty() {
                String::new()
            } else {
                format!(" · {}", escape(&turn.intent))
            };
            bits.push_str(&format!(
                r#"<div class="sum-line ai"><strong>{ai_name}{intent}</strong> {}</div>"#,
                escape(&turn.response)
            ));
        }
        if !bits.is_empty() {
            let number = turn.turn.map_or("?".to_string(), |n| n.to_string());
            turn_rows.push(format!(
                r#"<div class="sum-turn"><div class="eyebrow">Turn {number}</div>{bits}</div>"#
            ));
        }
    }
    let transcript_html = if turn_rows.is_empty() {
        r#"<p class="muted">No turns stored for this call.</p>"#.to_string()
    } else {
        turn_rows.join("\n")
    };
    let err_html = if analysis.error.is_empty() {
        String::new()
    } else {
        format!(
            r#"<p class="muted" style="margin-top:8px">Note: used offline summary ({})</p>"#,
            escape(&analysis.error)
        )
    };

    let scorecard_html = match (&analysis.roleplay_scorecard, roleplay) {
        (Some(score), true) => {
            let dims = [
                ("Greeting", score.greeting),
                ("Empathy", score.empathy),
                ("Discovery", score.discovery),
                ("Accuracy", score.accuracy),
                ("Control", score.control),
            ];
            let dim_html: String = dims
                .iter()
                .map(|(name, val)| {
                    let width = val.saturating_mul(10).clamp(0, 100);
                    format!(
                        r#"
            <div class="score-dim">
              <div class="score-dim-label">{name}</div>
              <div class="score-bar"><span style="width:{width}%"></span></div>
              <div class="score-dim-val">{val}/10</div>
            </div>
            "#
                    )
                })
                .collect();
            let list = |items: &[String]| {
                if items.is_empty() {
                    "<li class='muted'>—</li>".to_string()
                } else {
                    items
                        .iter()
                        .map(|x| format!("<li>{}</li>", escape(x)))
                        .collect()
                }
            };
            let strengths = list(&score.strengths);
            let improvements = list(&score.improvements);
            let overall = score.overall_score;
            let grade = escape(or(&score.grade, "—"));
            let scenario = escape(or(or(&score.scenario_label, &score.scenario), "practice"));
            let coach_notes = escape(or(&score.coach_notes, "—"));
            format!(
                r#"
        <section class="panel scorecard-panel" style="margin-bottom:14px">
          <div class="eyebrow">Train · Roleplay scorecard</div>
          <div class="score-hero">
            <div class="score-number">{overall}</div>
            <div>
              <div class="score-grade">Grade {grade}</div>
              <div class="muted">{scenario}</div>
            </div>
          </div>
          <div class="score-dims">{dim_html}</div>
          <div class="grid-2" style="margin-top:14px">
            <div><div class="eyebrow">Strengths</div><ul class="action-list">{strengths}</ul></div>
            <div><div class="eyebrow">Improvements</div><ul class="action-list">{improvements}</ul></div>
          </div>
          <div class="eyebrow" style="margin-top:14px">Coach notes</div>
          <p class="summary-text">{coach_notes}</p>
        </section>
        "#
            )
        }
        (None, true) => r#"
        <section class="panel scorecard-panel" style="margin-bottom:14px">
          <div class="eyebrow">Train · Roleplay scorecard</div>
          <p class="muted">Scorecard not generated yet. Refresh after hangup.</p>
        </section>
        "#
        .to_string(),
        _ => String::new(),
    };

    let primary_intent = escape(&or(&analysis.primary_intent, "unknown").replace('_', " "));
    let summary_text = escape(or(&analysis.summary, "No summary available yet."));
    let caller_goal = escape(or(&analysis.caller_goal, "—"));
    let outcome = escape(&or(or(&analysis.outcome, &session.status), "—").replace('_', " "));
    let started = escape(or(&session.started_at, "—"));
    let updated = escape(or(&session.updated_at, "—"));
    let body = format!(
        r#"
    {scorecard_html}
    <section class="panel" style="margin-bottom:14px">
      <div class="eyebrow">Primary intent</div>
      <div class="intent-hero">{primary_intent}</div>
      <div class="eyebrow">Quick summary</div>
      <p class="summary-text">{summary_text}</p>
      {err_html}
    </section>
    {recording_html}
    <div class="grid-2" style="margin-bottom:14px">
      <section class="panel">
        <div class="eyebrow">Caller goal</div>
        <p>{caller_goal}</p>
        <div class="eyebrow" style="margin-top:14px">Outcome</div>
        <p>{outcome}</p>
        <div class="eyebrow" style="margin-top:14px">Next best action</div>
        <p>{next_action}</p>
      </section>
      <section class="panel">
        <div class="eyebrow">Recovered information</div>
        {facts_table}
      </section>
    </div>
    <div class="grid-2" style="margin-bottom:14px">
      <section class="panel">
        <div class="eyebrow">Risks</div>
        <div>{risks_html}</div>
      </section>
      <section class="panel">
        <div class="eyebrow">Action items</div>
        {actions_html}
      </section>
    </div>
    <section class="panel" style="margin-bottom:14px">
      <div class="content-item-top">
        <div class="eyebrow">Follow-up email draft</div>
        <button type="button" class="secondary copy-content-btn" data-copy-body="{email}">Copy</button>
      </div>
      <pre class="draft">{email_shown}</pre>
    </section>
    <section class="panel" style="margin-bottom:14px">
      <div class="content-item-top">
        <div class="eyebrow">Internal handoff brief</div>
        <button type="button" class="secondary copy-content-btn" data-copy-body="{handoff}">Copy</button>
      </div>
      <pre class="draft">{handoff_shown}</pre>
    </section>
    {content_html}
    <section class="panel">
      <div class="eyebrow">Full conversation</div>
      {transcript_html}
      <p class="meta">session={sid} · started={started} · updated={updated}</p>
    </section>
    "#
    );
    let turns = session.turn_count;
    shell(
        ctx,
        Shell {
            active_id: if roleplay { "train" } else { "capture" },
            title: if roleplay {
                "Roleplay Scorecard"
            } else {
                "Summary Inform"
            },
            kicker: if roleplay {
                "Train · Scorecard"
            } else {
                "Capture · Detail"
            },
            subtitle: &format!("{} · {pill_l} · {turns} turns", escape(&line)),
            body: &body,
            actions_html: &format!(
                r#"<span class="pill pill-{pill_c}">{pill_l}</span><a class="btn btn-ghost" href="{}">Live Console</a><a class="btn btn-ghost" href="{}">Menu</a>"#,
                ctx.path("/console"),
                ctx.path("/")
            ),
            extra_js: COPY_SUMMARY_JS,
            page_title: Some(if roleplay {
                "Scorecard · Voice IN"
            } else {
                "Summary Inform · Voice IN"
            }),
            ..Default::default()
        },
    )
}

// ---------------------------------------------------------------------------
// Brand voice messages
// ---------------------------------------------------------------------------

/// The Brand page. `lang_labels` is the (code, label) list of TTS languages, in display order.
pub fn render_brand(ctx: &UiContext, lang_labels: &[(&str, &str)]) -> String {
    let extra_js = format!(
        "const API = {};\nconst LANG_LABELS = {};\n{BRAND_JS}",
        json_str(&ctx.path("/api/brand")),
        json_str_dict(lang_labels)
    );
    let accent_options: String = lang_labels
        .iter()
        .map(|(code, label)| {
            format!(
                r#"<option value="{}">{}</option>"#,
                escape(code),
                escape(label)
            )
        })
        .collect();
    let body = BRAND_BODY.replace("{{ACCENT_OPTIONS}}", &accent_options);
    shell(
        ctx,
        Shell {
            active_id: "brand",
            title: "Brand",
            kicker: "Skill · Voice messages",
            subtitle: "Create branded announcements, choose or clone a voice, then preview and download the audio.",
            body: &body,
            actions_html: &menu_and(ctx, "/create", "Create packs"),
            extra_css: BRAND_CSS,
            extra_js: &extra_js,
            page_title: Some("Brand · Voice IN"),
        },
    )
}

#[cfg(test)]
mod tests;
