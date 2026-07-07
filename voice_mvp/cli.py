from __future__ import annotations

import argparse
import json
from pathlib import Path
import uuid

from .audio import record_wav
from .config import load_settings
from .pipeline import run_interaction
from .xai_client import XAIClient


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Voice AI MVP CLI")
    subparsers = parser.add_subparsers(dest="command", required=True)

    subparsers.add_parser("run", help="Record, transcribe, reason, synthesize, and play one interaction.")
    subparsers.add_parser("check-env", help="Validate required local configuration.")
    record_parser = subparsers.add_parser("record-only", help="Record a WAV file without calling xAI.")
    record_parser.add_argument("--seconds", type=int, default=None, help="Override record duration.")
    smoke_parser = subparsers.add_parser("text-smoke", help="Test Grok and optional TTS without microphone input.")
    smoke_parser.add_argument("--text", required=True, help="Transcript text to send into the reasoning pipeline.")
    smoke_parser.add_argument(
        "--speak",
        action="store_true",
        help="Also synthesize the response to a WAV artifact.",
    )
    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    if args.command == "check-env":
        settings = load_settings()
        payload = settings.model_dump(mode="json")
        payload["xai_api_key"] = "***redacted***"
        print(json.dumps(payload, indent=2, default=str))
        return 0

    settings = load_settings()

    if args.command == "record-only":
        seconds = args.seconds or settings.record_seconds
        output_path = settings.artifacts_dir / "manual-recording.wav"
        record_wav(output_path, sample_rate=settings.sample_rate, duration_seconds=seconds)
        print(f"Saved recording to {output_path}")
        return 0

    if args.command == "text-smoke":
        client = XAIClient(settings)
        result = client.reason_over_transcript(args.text)
        payload = {"reasoning_result": result.model_dump(mode="json")}
        if args.speak:
            request_id = str(uuid.uuid4())
            output_path = settings.artifacts_dir / "text-smoke-output.wav"
            synthesis = client.synthesize_to_wav(request_id, result.response, output_path)
            payload["synthesis_event"] = synthesis.model_dump(mode="json")
        print(json.dumps(payload, indent=2))
        return 0

    if args.command == "run":
        interaction = run_interaction(settings)
        print(json.dumps(interaction.model_dump(mode="json"), indent=2))
        return 0

    parser.print_help()
    return 1
