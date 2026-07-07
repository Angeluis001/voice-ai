# Voice substrate — wire, perception, synthesis

**Date:** 2026-04-30
**Status:** Locked. Reconciles operator decisions + voice-research-agent guidance.
**Companion docs:** [`2026-04-30.local-model-selection.md`](./2026-04-30.local-model-selection.md), [`2026-04-30.gpu-vm-migration.md`](./2026-04-30.gpu-vm-migration.md)
**Authoritative memory:** `project_voice_substrate.md`, `project_val_switch_omni_channel.md`, `project_model_stack_lock.md`

## TL;DR

- **Wire substrate (locked):** Kamailio (SIP edge) + FreeSWITCH (media) + Jambonz (webhook control plane) + MeshCentral (fleet RMM + SOP screen capture). Asterisk + LiveKit are out, permanently.
- **Voice IN — split:**
  - **Device-side** (laptop, kiosk, Physical AI): no STT sidecar. **Gemma 4 E4B's Conformer encoder** takes audio in directly.
  - **Telephony** (audio in FreeSWITCH on a different host): **faster_whisper sidecar** transcribes; hands transcript to val-switch.
- **Voice OUT — four-tier cascade:**
  1. **Kokoro** (default, already in [`val-core/src/voice_output.rs`](../../v_valos/source/fusion-engines/libraries/val-core/src/voice_output.rs)) with four channels: chief_of_staff / hey_val / alert / field_worker.
  2. **Piper** for streaming + edge — when real-time turn-taking matters (operator interrupts mid-response).
  3. **XTTS-v2** for voice cloning + per-persona — Phase 9 vWorker voice work.
  4. **ElevenLabs / Cartesia** for outbound brand-grade — privacy + cost gated.
- **Conversational reasoning** — same Gemma 4 tier on the device handles audio in + intent + response. No separate "voice LLM."

## The three jobs voice does

Voice isn't one model. It's a small fleet covering three distinct jobs.

### Job 1 — Voice IN (perception + intent)

Two paths depending on where the audio physically lives.

**Device-side path** (operator laptop, kiosk, Physical AI device):

```
microphone (16kHz PCM) → Gemma 4 E4B (Conformer encoder) → {transcript, intent, response}
```

One inference call. The model's Conformer takes audio bytes in, emits structured intent + transcript + response. ASR + audio understanding + intent classification + reasoning collapse into one component.

This is the architectural simplification. Previous designs had Whisper as a separate STT container called from val-bay's STT models lane. Gemma 4 E4B's native audio means **no STT sidecar on devices.**

**Telephony path** (audio in FreeSWITCH on a different host, not at the device):

```
PSTN call → Kamailio → FreeSWITCH → audio fork → faster_whisper sidecar → transcript → val-switch → CoS / val-bay
```

faster_whisper (CTranslate2 backend) is the default. Options ranked:

| Sidecar | When |
|---|---|
| **faster_whisper** (CTranslate2) | Default. Best speed/quality on CPU; GPU when available. |
| **whisper.cpp** (GGML quantized) | When the host has no GPU and tight memory budget. |
| **WhisperLive** | When streaming partial transcripts matter (barge-in / interrupt detection during a call). |
| **Cloud fallback** (Deepgram / AssemblyAI / OpenAI Whisper API) | Privacy-policy gated. Best-in-class streaming when the call's sensitivity tier allows. |

Telephony **can't** use Gemma 4's native audio because the audio bytes never reach the device-tier model — they're trapped in FreeSWITCH's media stack on the HQ side. Sidecar pattern stays for this lane only.

### Job 2 — Voice OUT (speech synthesis)

Four tiers, picked per call by val-switch (or whatever orchestrator initiates the speech).

**Tier 1 — Kokoro (local default)**

Already coded in [`v_valos/source/fusion-engines/libraries/val-core/src/voice_output.rs`](../../v_valos/source/fusion-engines/libraries/val-core/src/voice_output.rs). Four channels mapped to Kokoro voices:

| Channel | Voice | Use |
|---|---|---|
| `chief_of_staff` | `af_bella` (professional, clear) | formal business responses |
| `hey_val` | `af_nicole` (warm, conversational) | conversational CoS chat |
| `alert` | `am_adam` (authoritative, urgent) | urgent notifications |
| `field_worker` | `am_michael` (clear, concise) | field-worker instructions |

Kokoro is the default for everything that doesn't need streaming, cloning, or brand-grade.

**Tier 2 — Piper (streaming + edge)**

When real-time turn-taking matters (operator interrupts mid-response, tight latency budget on a phone call), Piper's <100ms first-byte latency wins over Kokoro. Lighter model — runs on edge devices including the Physical AI fleet.

Trade-off: voice quality slightly behind Kokoro for the same compute. Use when latency > polish.

**Tier 3 — XTTS-v2 (Coqui — voice cloning)**

When Phase 9's vWorker voice ships (each persona narrating in its own voice), XTTS-v2 enables cloning from a short reference sample. One TTS engine, many voices. Bigger model, GPU-friendly.

Don't deploy until vWorker persona-voice work is in scope. Until then this is reserved.

**Tier 4 — ElevenLabs / Cartesia (cloud, brand-grade)**

For high-quality client-facing only — outbound calls (e.g., debt-collection where voice quality is brand-grade). Privacy + cost gated; val-switch routes here only when:
- The call's sensitivity tier allows cloud TTS
- The cost budget on the calling pulse covers it
- An operator-stated brand preference flags this voice as "the brand voice"

### Job 3 — Conversational reasoning (between IN and OUT)

Whatever Gemma 4 tier is on the device. Same model handles intent classification on the inbound utterance AND the response generation. No separate "voice LLM."

For HQ-resident telephony reasoning (after faster_whisper sidecar transcribes), reasoning routes through val-bay's gateway like any other heavy pulse — see local-model-selection doc. Default lands on Gemma 4 26B MoE A4B; cloud escape to Claude / Qwen / DeepSeek if the workload needs it.

## Wire substrate (locked, not yet built)

These four together cover voice + multi-channel comms + remote-control fleet management. Built as Ring 0 dockers when val-switch ships:

| Component | Role | Why |
|---|---|---|
| **Kamailio** | SIP edge / SBC | Every trunk lands here. NAT, registration, rate-limit, fraud guard. PSTN in v1. |
| **FreeSWITCH** | Media engine | RTP, transcoding, recording, conferencing, voicemail, IVR primitives. |
| **Jambonz** | Webhook control plane | TwiML-style application logic via HTTP webhooks; speaks to FS + Kamailio under the hood. ValOS-side handlers in val-switch. |
| **MeshCentral** | Fleet RMM + SOP capture | Engineer-side remote control of devices; operator-side screen + camera capture during work sessions (pairs with FreeSWITCH audio for SOP loops). |

Each gets a val-node sidecar (per `project_peer_mesh_architecture.md`) so they appear as mesh peers — `valos-kamailio`, `valos-freeswitch`, `valos-jambonz`, `valos-meshcentral`. Application-side calls are mesh RPC; the sidecars translate to local HTTP/AMI/etc.

## val-switch responsibilities (when it ships)

Per `project_val_switch_omni_channel.md` — val-switch is the omni-channel comms engine. Voice-specific responsibilities:

- **Routing** — given an outbound voice call request, pick the right channel (PSTN trunk, MS Graph DR, BYO carrier) based on actor preferences + presence + caller-ID locale + cost.
- **Threading** — stitch voice transcripts (from device-side or telephony STT) into the same conversation entity that holds email + Teams + WhatsApp messages.
- **Policy** — business hours, retry-on-no-answer with fallback channel, escalation, cost-budget enforcement.
- **Voice-as-security gates** — Voisek partnership integration: voiceprint biometric + anti-deepfake liveness check before connecting sensitive calls. (Not Whisper — Voisek runs alongside.)
- **TTS channel selection** — when val-switch initiates speech (greeting, IVR menu, voicemail-leave), pick the right Kokoro channel per context.

## Architectural simplification crystallized

| Was (pre-Gemma 4) | Now (Gemma 4 E4B locked) |
|---|---|
| Whisper sidecar on every device | None — device-side audio bypasses STT |
| Whisper sidecar in telephony pipeline | Stays — telephony audio can't reach the device-tier model |
| STT-as-skill (val-bay routes audio chunks to a remote Whisper) | Dead pattern — audio is local to the model on devices, sidecar-handled on telephony |
| TTS via shell hooks or ad-hoc Kokoro calls | Canonical: `val-core::voice_output::VoiceChannel` — extend for Piper / XTTS-v2 / ElevenLabs additions |

## Phase rollout

1. **Now (no GPU VM yet):** existing Ollama + CPU inference; voice work blocked on hardware.
2. **GPU VM provisioned (Phase 4 work):** Pull Gemma 4 26B MoE A4B for HQ; pull Kokoro; pull faster_whisper for telephony sidecar (held until val-switch ships).
3. **Laptop deployable (Phase 4):** bundle Gemma 4 E4B in the val-desk installer; first device-side voice round-trip on operator laptop.
4. **val-switch first ship:** Kamailio + FS + Jambonz + MeshCentral docker stack; faster_whisper telephony sidecar online; first PSTN call works.
5. **Phase 9 (later):** XTTS-v2 + per-persona voice cloning for vWorker speech.

## How to apply

- Don't reintroduce Whisper as a device-side STT — that pattern is dead.
- Don't build separate audio-handling code paths in val-agent — Gemma 4 E4B's API takes audio bytes; use it.
- Don't bypass `val-core::voice_output::VoiceChannel` when adding TTS variants — extend the trait, register Piper/XTTS-v2/ElevenLabs implementations alongside the existing Kokoro one.
- Telephony STT sidecar is configured per Ring 0 install; default to faster_whisper, swap to whisper.cpp / WhisperLive / cloud per the routing/sensitivity policy.
- Privacy-tier on telephony calls drives whether cloud STT (Deepgram etc.) or cloud TTS (ElevenLabs etc.) is even a candidate.
