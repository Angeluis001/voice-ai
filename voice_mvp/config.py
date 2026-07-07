from __future__ import annotations

from pathlib import Path
from pydantic import BaseModel, Field, SecretStr
from dotenv import load_dotenv
import os


load_dotenv()


class Settings(BaseModel):
    xai_api_key: SecretStr = Field(alias="XAI_API_KEY")
    xai_base_url: str = Field(default="https://api.x.ai/v1", alias="XAI_BASE_URL")
    language: str = Field(default="en", alias="VOICE_MVP_LANGUAGE")
    stt_keyterm: str | None = Field(default=None, alias="VOICE_MVP_STT_KEYTERM")
    grok_model: str = Field(default="grok-4.3", alias="VOICE_MVP_GROK_MODEL")
    tts_voice: str = Field(default="eve", alias="VOICE_MVP_TTS_VOICE")
    record_seconds: int = Field(default=5, alias="VOICE_MVP_RECORD_SECONDS")
    sample_rate: int = Field(default=16000, alias="VOICE_MVP_SAMPLE_RATE")
    artifacts_dir: Path = Field(default=Path("artifacts"))
    logs_dir: Path = Field(default=Path("logs"))


def load_settings() -> Settings:
    payload = {
        "XAI_API_KEY": os.getenv("XAI_API_KEY", ""),
        "XAI_BASE_URL": os.getenv("XAI_BASE_URL", "https://api.x.ai/v1"),
        "VOICE_MVP_LANGUAGE": os.getenv("VOICE_MVP_LANGUAGE", "en"),
        "VOICE_MVP_STT_KEYTERM": os.getenv("VOICE_MVP_STT_KEYTERM"),
        "VOICE_MVP_GROK_MODEL": os.getenv("VOICE_MVP_GROK_MODEL", "grok-4.3"),
        "VOICE_MVP_TTS_VOICE": os.getenv("VOICE_MVP_TTS_VOICE", "eve"),
        "VOICE_MVP_RECORD_SECONDS": os.getenv("VOICE_MVP_RECORD_SECONDS", "5"),
        "VOICE_MVP_SAMPLE_RATE": os.getenv("VOICE_MVP_SAMPLE_RATE", "16000"),
    }
    settings = Settings.model_validate(payload)
    if not settings.xai_api_key.get_secret_value():
        raise ValueError("XAI_API_KEY is missing. Add it to your local environment or .env file.")
    settings.artifacts_dir.mkdir(parents=True, exist_ok=True)
    settings.logs_dir.mkdir(parents=True, exist_ok=True)
    return settings
