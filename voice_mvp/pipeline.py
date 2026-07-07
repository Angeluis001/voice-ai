from __future__ import annotations

from pathlib import Path
from datetime import UTC, datetime

from .audio import play_wav, record_wav
from .config import Settings
from .contracts import InteractionLog
from .logging_utils import append_jsonl
from .xai_client import XAIClient


def _stamp() -> str:
    return datetime.now(UTC).strftime("%Y%m%d-%H%M%S")


def run_interaction(settings: Settings) -> InteractionLog:
    stamp = _stamp()
    capture_path = settings.artifacts_dir / f"{stamp}-input.wav"

    record_wav(
        output_path=capture_path,
        sample_rate=settings.sample_rate,
        duration_seconds=settings.record_seconds,
    )

    client = XAIClient(settings)
    transcript_event = client.transcribe_file(capture_path)
    reasoning_result = client.reason_over_transcript(transcript_event.transcript)
    output_path = settings.artifacts_dir / f"{stamp}-output.wav"
    synthesis_event = client.synthesize_to_wav(
        request_id=transcript_event.request_id,
        text=reasoning_result.response,
        output_path=output_path,
    )
    play_wav(output_path)

    interaction = InteractionLog(
        request_id=transcript_event.request_id,
        transcript_event=transcript_event,
        reasoning_result=reasoning_result,
        synthesis_event=synthesis_event,
    )
    append_jsonl(settings.logs_dir / "interactions.jsonl", interaction.model_dump(mode="json"))
    return interaction
