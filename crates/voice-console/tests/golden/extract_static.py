"""Lift the CSS/JS blocks out of voice_in_agent/console_ui.py byte-for-byte.

Run from the repo root:  .venv/Scripts/python.exe crates/voice-console/tests/golden/extract_static.py
Writes crates/voice-console/static/*. Re-run whenever the Python UI assets change.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

os.environ.pop("VOICE_IN_BASE_PATH", None)
sys.path.insert(0, os.getcwd())

from voice_in_agent import console_ui as ui  # noqa: E402
from voice_in_agent.models import CallPostAnalysis, LiveCallSession  # noqa: E402

OUT = Path("crates/voice-console/static")
OUT.mkdir(parents=True, exist_ok=True)


def write(name: str, text: str) -> None:
    (OUT / name).write_bytes(text.encode("utf-8"))
    print(f"{name}: {len(text)} chars")


def between(text: str, start: str, end: str) -> str:
    i = text.index(start) + len(start)
    j = text.index(end, i)
    return text[i:j]


captured: dict = {}
real_shell = ui.shell


def fake_shell(**kwargs):
    captured.clear()
    captured.update(kwargs)
    return real_shell(**kwargs)


ui.shell = fake_shell

base = ui._base_css()
write("base.css", base)

login = ui.render_login()
write("login.css", between(login, "<style>\n" + base + "\n", "  </style>"))

js = ui._console_js()
prefix = '\n    const BASE_PATH = "";\n    function P(u) { return BASE_PATH + u; }\n    '
assert js.startswith(prefix), js[:120]
write("console.js", js[len(prefix):])

ui.render_brand()
write("brand.css", captured["extra_css"])
bjs = captured["extra_js"]
marker = ";\n"
first = bjs.index(marker) + len(marker)
second = bjs.index(marker, first) + len(marker)
assert bjs[:first] == 'const API = "/api/brand";\n', bjs[:first]
assert bjs[first:second].startswith("const LANG_LABELS = {"), bjs[first:second]
write("brand.js", bjs[second:])
opts = "".join(
    f'<option value="{ui.escape(code)}">{ui.escape(label)}</option>' for code, label in ui.TTS_LANGUAGES
)
body = captured["body"]
assert body.count(opts) == 1
write("brand_body.html", body.replace(opts, "{{ACCENT_OPTIONS}}"))

ui.render_create([])
write("copy_create.js", between(captured["body"], "<script>", "</script>"))

ui.render_summary(LiveCallSession(session_id="x"), CallPostAnalysis())
write("copy_summary.js", captured["extra_js"])
