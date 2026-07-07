from __future__ import annotations

from datetime import UTC, datetime
from pydantic import BaseModel, Field


def utc_now_iso() -> str:
    return datetime.now(UTC).isoformat()


class TranscriptWord(BaseModel):
    text: str
    start: float
    end: float
    speaker: int | None = None


class TranscriptEvent(BaseModel):
    request_id: str
    timestamp: str = Field(default_factory=utc_now_iso)
    transcript: str
    language: str
    duration_seconds: float
    provider: str = "xai-stt"
    words: list[TranscriptWord] = Field(default_factory=list)


class ReasoningResult(BaseModel):
    transcript: str = Field(description="Verbatim or lightly normalized user utterance.")
    intent: str = Field(description="Short machine-readable intent label such as greeting or ask_status.")
    response: str = Field(description="Natural language assistant response to speak back to the user.")


class SynthesisEvent(BaseModel):
    request_id: str
    timestamp: str = Field(default_factory=utc_now_iso)
    provider: str = "xai-tts"
    voice_id: str
    codec: str
    sample_rate: int
    file_path: str


class InteractionLog(BaseModel):
    request_id: str
    transcript_event: TranscriptEvent
    reasoning_result: ReasoningResult
    synthesis_event: SynthesisEvent
