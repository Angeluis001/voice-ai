from __future__ import annotations

from pathlib import Path
import json
import uuid

import requests
from openai import OpenAI

from .config import Settings
from .contracts import ReasoningResult, SynthesisEvent, TranscriptEvent, TranscriptWord


SYSTEM_PROMPT = """You are the voice orchestration brain for a Windows desktop MVP.
Return strict JSON with:
- transcript: the user's utterance, normalized only lightly if needed
- intent: a short snake_case intent label
- response: a concise spoken reply that sounds natural aloud

Keep response helpful, brief, and safe for operator testing.
"""


class XAIClient:
    def __init__(self, settings: Settings) -> None:
        self.settings = settings
        self.base_url = settings.xai_base_url.rstrip("/")
        self.http = requests.Session()
        api_key = settings.xai_api_key.get_secret_value()
        self.http.headers.update({"Authorization": f"Bearer {api_key}"})
        self.openai = OpenAI(api_key=api_key, base_url=self.base_url)

    def transcribe_file(self, audio_path: Path) -> TranscriptEvent:
        request_id = str(uuid.uuid4())
        data: list[tuple[str, str]] = [
            ("format", "true"),
            ("language", self.settings.language),
        ]
        if self.settings.stt_keyterm:
            data.append(("keyterm", self.settings.stt_keyterm))

        with audio_path.open("rb") as audio_handle:
            files = {"file": (audio_path.name, audio_handle, "audio/wav")}
            response = self.http.post(f"{self.base_url}/stt", data=data, files=files, timeout=120)
        response.raise_for_status()
        payload = response.json()

        words = [
            TranscriptWord(
                text=word["text"],
                start=word["start"],
                end=word["end"],
                speaker=word.get("speaker"),
            )
            for word in payload.get("words", [])
        ]
        return TranscriptEvent(
            request_id=request_id,
            transcript=payload["text"],
            language=payload.get("language", self.settings.language),
            duration_seconds=payload.get("duration", 0.0),
            words=words,
        )

    def reason_over_transcript(self, transcript: str) -> ReasoningResult:
        completion = self.openai.beta.chat.completions.parse(
            model=self.settings.grok_model,
            messages=[
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": transcript},
            ],
            response_format=ReasoningResult,
        )
        parsed = completion.choices[0].message.parsed
        if parsed is None:
            raw_content = completion.choices[0].message.content or ""
            return ReasoningResult.model_validate_json(raw_content)
        return parsed

    def synthesize_to_wav(self, request_id: str, text: str, output_path: Path) -> SynthesisEvent:
        response = self.http.post(
            f"{self.base_url}/tts",
            headers={"Content-Type": "application/json"},
            data=json.dumps(
                {
                    "text": text,
                    "voice_id": self.settings.tts_voice,
                    "language": self.settings.language,
                    "output_format": {
                        "codec": "wav",
                        "sample_rate": 24000,
                    },
                }
            ),
            timeout=120,
        )
        response.raise_for_status()
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_bytes(response.content)
        return SynthesisEvent(
            request_id=request_id,
            voice_id=self.settings.tts_voice,
            codec="wav",
            sample_rate=24000,
            file_path=str(output_path),
        )
