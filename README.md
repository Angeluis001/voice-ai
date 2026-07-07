# Voice AI MVP

Desktop voice MVP for Windows using xAI STT, Grok structured outputs, and xAI TTS.

## What It Does

The MVP records a short microphone utterance, sends it to xAI speech-to-text, asks Grok for a structured response, synthesizes speech, and plays the response locally as a WAV file.

Pipeline:

`mic -> STT -> Grok -> TTS -> WAV playback`

## Local Setup

1. Create a virtual environment.
2. Install dependencies from `requirements.txt`.
3. Copy `.env.example` to `.env`.
4. Put your local `XAI_API_KEY` in `.env`.
5. Run `python -m voice_mvp run`.

## Notes

- The actual API key should never be committed.
- The MVP requests TTS as `wav`, which allows Windows playback through `winsound`.
- Docker is not required for Phase 1.

## Planned Next Step

After the CLI path works, we can add:

- streaming or realtime mode
- better device selection
- local Kokoro adapter
- telephony bridge preparation

## SIP Foundation

The main project direction is SIP-first. Phase 1 artifacts for Telair validation live in:

- `.env.sip.example`
- `docs/sip/telair-validation-checklist.md`
- `docs/sip/softphone-profile.md`
- `.env.sip.stack.example`
- `docker/sip-stack/docker-compose.yml`
- `docs/sip/docker-stack-runbook.md`

Recommended order:

1. Fill a local `.env.sip` from `.env.sip.example`
2. Configure a softphone using `docs/sip/softphone-profile.md`
3. Execute the checklist in `docs/sip/telair-validation-checklist.md`
4. Move to Docker SBC/media stack after carrier validation passes

## Docker SIP Stack

The MD-aligned stack scaffold now lives under `docker/sip-stack/` and models:

- `Kamailio` as SIP edge
- `drachtio-server` plus a small Node `drachtio-srf` app as the application control plane
- `FreeSWITCH` as media support for the voice application path

This is the authoritative path for the project because it keeps the MD shape around `Kamailio` and `FreeSWITCH` while replacing Jambonz with an open-source control layer. This scaffold is intentionally bootstrap-oriented and still requires public or routed SIP plus RTP reachability planning.
