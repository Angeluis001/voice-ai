"""Generate golden HTML from the Python console UI for the Rust port to match.

Run from the repo root (no auth env vars set):
    .venv/Scripts/python.exe crates/voice-console/tests/golden/make_golden.py
It re-invokes itself with VOICE_IN_BASE_PATH=/ops for the proxied variant.
All data here is synthetic.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path("crates/voice-console/tests/golden")
VARIANT = os.environ.get("GOLDEN_VARIANT")

if VARIANT is None:
    for variant, base in (("root", ""), ("ops", "/ops")):
        env = dict(os.environ, GOLDEN_VARIANT=variant)
        env.pop("VOICE_IN_BASE_PATH", None)
        if base:
            env["VOICE_IN_BASE_PATH"] = base
        subprocess.run([sys.executable, __file__], check=True, env=env)
    sys.exit(0)

sys.path.insert(0, os.getcwd())

from voice_in_agent import console_ui as ui  # noqa: E402
from voice_in_agent.models import (  # noqa: E402
    CallActionItem,
    CallContentArtifact,
    CallContentPack,
    CallPostAnalysis,
    CallStakeholder,
    LiveCallSession,
    RoleplayScorecard,
    VoiceInResult,
)

T0 = "2026-07-08T01:02:03.000000+00:00"
T1 = "2026-07-08T01:05:09.123456+00:00"


def turn(n, transcript, intent, response, **kw):
    return VoiceInResult(
        source="telephony", request_id=f"req-{n}", timestamp=T0, transcript=transcript,
        intent=intent, response=response, turn=n, **kw,
    )


def build_sessions() -> list[LiveCallSession]:
    long_body = "Checklist line with <tags> & \"quotes\" — é.\n" * 40
    return [
        LiveCallSession(
            session_id="sess-active-enquiry-0001", call_id="call-1", is_active=True, started_at=T0, updated_at=T1,
            status="active", latest_intent="new_enquiry", latest_phase="transcribed",
            latest_transcript="I've got an <unpaid> invoice & it's \"overdue\"",
            latest_response="Thanks — what's the approximate amount owed?",
            target_number="9198", prompt_profile="sop_enquiries_realtime",
            slots={"debt_type": "commercial invoice", "amount": None, "staff_name": "unknown", "reference": "INV-<7>"},
            turn_count=2, latest_latency_ms=640,
            turns=[
                turn(0, "", "opening_greeting", "Welcome to the enquiries line."),
                turn(1, "I've got an <unpaid> invoice", "new_enquiry", "Thanks — what's the amount?"),
            ],
        ),
        LiveCallSession(
            session_id="sess-transferred-0002-with-a-long-identifier", is_active=False, started_at=T0, updated_at=T1,
            status="transferred", latest_intent="transfer_request", latest_action="transfer", latest_phase="completed",
            latest_transcript="Can I talk to a person please", latest_response="Connecting you now.",
            target_number="9198", prompt_profile="sop_enquiries",
            slots={"debt_type": "cleaning invoice", "amount": 6000, "action": "transfer"},
            turn_count=4, has_recording=True, has_post_analysis=True, recording_path="/rec/2.wav",
            handoff_brief="  Caller: Dana & Co <dana@example.test>\nOwed: $6,000 'cleaning'  ",
            turns=[
                turn(0, "", "", "Hello, enquiries."),
                turn(1, "Someone owes me money", "new_enquiry", "What type of debt is it?"),
                turn(2, "A cleaning invoice, six grand", "", "Got it."),
                turn(3, "", "", ""),
                VoiceInResult(source="telephony", request_id="req-x", timestamp=T0, transcript="Can I talk to a person",
                              intent="transfer_request", response="Connecting you <now>."),
            ],
            post_analysis=CallPostAnalysis(
                primary_intent="new_debt_enquiry", summary="Caller has a $6,000 unpaid cleaning invoice & wants a person.",
                outcome="transferred_to_staff", caller_goal="Recover an unpaid 'commercial' invoice",
                key_facts={"amount": "$6,000", "debtor_name": "Acme <Pty>", "disputed": False, "age_days": 45,
                           "ratio": 0.5, "notes": None, "email": "unknown", "tags": ["a", "b"]},
                recovered_slots={"debt_type": "cleaning invoice", "amount": 6000, "staff_name": "", "reference": "null"},
                turn_intents=["new_enquiry", "transfer_request"], risks=["caller_impatient", "amount_unverified"],
                action_items=[CallActionItem(task="Call back & confirm <amount>", due="tomorrow 12pm"),
                              CallActionItem(owner="system", task="Send LOD template")],
                next_best_action="call_back_today", stakeholders=[CallStakeholder(name="Dana")],
                follow_up_email_draft="Hi Dana,\n\nThanks for calling — \"next steps\" below.\n",
                internal_handoff_brief="analysis brief (unused when session brief set)",
                generated_at=T1,
            ),
            content=CallContentPack(artifacts=[
                CallContentArtifact(key="email", title="Follow-up <email>", body="Hi Dana & team,\nIt's sorted.", content_type="email"),
                CallContentArtifact(key="sms", title="SMS", body="We'll call you back.", content_type="sms"),
                CallContentArtifact(key="checklist", title="Checklist", body=long_body, content_type="checklist"),
                CallContentArtifact(key="handoff", title="Handoff", body="x" * 1200),
                CallContentArtifact(key="extra", title="Fifth (hidden on Create)", body="five"),
            ], generated_at=T1),
        ),
        LiveCallSession(
            session_id="sess-roleplay-0003", is_active=False, started_at=T0, updated_at=T1, status="completed",
            latest_intent="roleplay_turn", latest_phase="completed", latest_transcript="Thanks for your time",
            latest_response="Okay, bye.", target_number="9199", prompt_profile="roleplay", mode="roleplay",
            roleplay_scenario="promise", roleplay_label="Promise to pay — Sochelkara", turn_count=3,
            has_recording=True, has_post_analysis=True,
            turns=[
                turn(0, "", "roleplay_customer_opening", "Hello? Who's this?"),
                turn(1, "Hi, it's Sam from recoveries", "roleplay_turn", "Oh. What's this about?"),
                turn(2, "Your account is overdue", "", ""),
            ],
            post_analysis=CallPostAnalysis(
                primary_intent="roleplay_practice", summary="Practice call.", outcome="completed",
                roleplay_scorecard=RoleplayScorecard(
                    scenario="promise", scenario_label="Promise to pay", overall_score=82, grade="B",
                    greeting=9, empathy=7, discovery=12, accuracy=-1, control=8,
                    strengths=["Clear greeting", "Used the caller's name"], improvements=["Confirm <amount> & date"],
                    coach_notes="Good control; confirm the promise date.",
                ),
            ),
        ),
        LiveCallSession(
            session_id="sess-roleplay-pending-0004", is_active=True, started_at=T0, updated_at="", status="active",
            latest_phase="processing", target_number="9199gate", prompt_profile="Roleplay",
            roleplay_scenario="gate", turn_count=1,
        ),
        LiveCallSession(
            session_id="sess-callback-0005", is_active=False, started_at="", updated_at="", status="callback_requested",
            latest_intent="callback_request", latest_action="callback", target_number="9197", turn_count=2,
            has_post_analysis=True,
        ),
        LiveCallSession(
            session_id="sess-unsupported-0006", status="completed", latest_intent="unsupported_debt",
            latest_action="decline_debt_type", target_number="2900", turn_count=5,
            slots={"debt_type": "residential rent", "amount": "unknown"},
            post_analysis=CallPostAnalysis(status="failed", error="model <timeout>", roleplay_scorecard=RoleplayScorecard()),
            content=CallContentPack(artifacts=[]),
        ),
        LiveCallSession(
            session_id="sess-fees-0007", status="failed", latest_intent="Fees_Question", latest_phase="failed",
            target_number="9197", prompt_profile="pilot", turn_count=1,
        ),
        LiveCallSession(
            session_id="sess-intake-0008", is_active=True, status="active", latest_intent="new_enquiry",
            latest_phase="some_custom_phase 2nd", slots={"debt_type": "trade debt", "amount": 1250.5, "staff_name": "Alex"},
            turn_count=3,
        ),
        LiveCallSession(
            session_id="sess-standby-0009", status="", latest_transcript="word " * 120, target_number="9199",
            prompt_profile="sop_enquiries", turn_count=0, latest_phase="",
        ),
        LiveCallSession(
            session_id="sess-live-0010", is_active=True, status="active", target_number="2900", latest_phase="reasoned",
            latest_intent="", turn_count=1, slots={"amount": 300, "action": "continue"},
        ),
        LiveCallSession(
            session_id="sess-debt-only-0011", status="recent", target_number="9198", slots={"debt_type": "loan", "amount": 0},
            mode="roleplay", turn_count=7,
        ),
        LiveCallSession(
            session_id="sess-missing-amount-0012", status="Completed ", latest_phase=" Queued", target_number="9198",
            slots={"debt_type": "loan", "amount": ""}, turn_count=2, has_recording=True,
        ),
    ]


PACK = {
    "id": "sbr",
    "roleplay": {
        "default_scenario": "paid",
        "scenarios": {
            "paid": {"label": "Already paid — Brett", "blurb": "Brett <Anthony> · $100 paid", "track": "collections"},
            "gate": {"label": "", "opening": "Hello, who's calling?", "track": "collections"},
            "fees": {"blurb": "Fees & commission questions"},
            "bad": "not a dict",
        },
        "tree": {
            "title": "Collections call <script>", "subtitle": "", "identify": "Hi, it's Sam from SBR",
            "purpose": "Outstanding account & next steps", "callback": "",
            "staff_role": "You are the collector", "ai_role": "",
            "branches": [
                {"when": "Says they've paid", "do": "Ask for receipt", "dial": "9199"},
                {"when": "Gatekeeper", "do": "Leave a message"},
                "junk",
                {"dial": 9199},
            ],
        },
    },
}
PACK_NO_TREE = {"roleplay": {"scenarios": {}, "tree": {"branches": "nope"}}}
USER = {"name": "Angel O'Test", "email": "angel+ops@example.test"}

sessions = build_sessions()
ended = [s for s in sessions if not s.is_active]
out = HERE / VARIANT
out.mkdir(parents=True, exist_ok=True)

if VARIANT == "root":
    fixtures = {
        "sessions": [s.model_dump(mode="json") for s in sessions],
        "pack": PACK,
        "pack_no_tree": PACK_NO_TREE,
        "lang_labels": [[c, l] for c, l in ui.TTS_LANGUAGES],
        "user": USER,
    }
    (HERE / "fixtures.json").write_bytes(
        (json.dumps(fixtures, indent=1, ensure_ascii=False) + "\n").encode("utf-8")
    )

pages: dict[str, str] = {
    "login": ui.render_login(),
    "login_error": ui.render_login(error="Sign-in <failed> & 'expired'"),
    "brand": ui.render_brand(),
    "train_pack": ui.render_train(sessions, pack=PACK),
    "train_pack_no_tree": ui.render_train([], pack=PACK_NO_TREE),
}
for set_name, subset in (("all", sessions), ("ended", ended), ("empty", [])):
    pages[f"home_{set_name}"] = ui.render_home(subset)
    pages[f"console_{set_name}"] = ui.render_console(subset)
    pages[f"assist_{set_name}"] = ui.render_assist(subset)
    pages[f"record_{set_name}"] = ui.render_record(subset)
    pages[f"capture_{set_name}"] = ui.render_capture(subset)
    pages[f"train_{set_name}"] = ui.render_train(subset)
    pages[f"create_{set_name}"] = ui.render_create(subset)
for i, s in enumerate(sessions):
    pages[f"summary_{i:02d}"] = ui.render_summary(s, s.post_analysis or CallPostAnalysis())

ui._session_user = lambda: USER
pages["home_user"] = ui.render_home(sessions)
ui._session_user = lambda: {"name": "", "email": None}
pages["home_user_blank"] = ui.render_home([])

for name, html in pages.items():
    (out / f"{name}.html").write_bytes(html.encode("utf-8"))
print(f"{VARIANT}: wrote {len(pages)} pages (BASE_PATH={ui.BASE_PATH!r})")
