from __future__ import annotations

from pathlib import Path
import wave
import winsound

import numpy as np
import sounddevice as sd


def record_wav(output_path: Path, sample_rate: int, duration_seconds: int) -> Path:
    frames = int(sample_rate * duration_seconds)
    recording = sd.rec(frames, samplerate=sample_rate, channels=1, dtype="int16")
    sd.wait()

    output_path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(output_path), "wb") as wav_file:
        wav_file.setnchannels(1)
        wav_file.setsampwidth(2)
        wav_file.setframerate(sample_rate)
        wav_file.writeframes(np.asarray(recording).tobytes())
    return output_path


def play_wav(path: Path) -> None:
    winsound.PlaySound(str(path), winsound.SND_FILENAME)
