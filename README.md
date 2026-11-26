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

## Voice IN Agent MVP

A separate browser-based MVP now lives in `voice_in_agent/`.

Purpose:

- upload a WAV file and get a structured `{ transcript, intent, response }`
- inspect recent telephony pilot artifacts in the same schema
- present a near-realtime employee-assist session board for live calls
- validate the product shape without depending on ValOS

Run it locally (or for remote access via port forwarding):

1. Install the updated dependencies from `requirements.txt`
2. Ensure `XAI_API_KEY` is present in `.env`
3. Start the app bound to all interfaces:

```powershell
# Recommended: binds 0.0.0.0:8000 so LAN and port-forward clients can connect
.\scripts\start-voice-in-agent.ps1
```

Or manually:

```powershell
.\.venv\Scripts\python.exe -m uvicorn voice_in_agent.app:app --host 0.0.0.0 --port 8000 --reload
```

4. Open:
   - Local: `http://127.0.0.1:8000`
   - Same network: `http://<this-pc-lan-ip>:8000`
   - Anywhere (after port forward): `http://<public-ip-or-dns>:8000`

### Remote access / port forwarding

To open the employee console from outside this PC:

1. **App must listen on `0.0.0.0`** (not only `127.0.0.1`) — use the start script above.
2. **Windows Firewall** must allow inbound TCP `8000` (elevated PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -File ".\scripts\setup-windows-firewall.ps1"
```

3. **Router / NAT**: forward public TCP `8000` → this PC’s LAN IP, TCP `8000`.
4. Browse to `http://<public-ip>:8000` (or your DNS name).

**Security:** production ops at `https://voice.valtience.com/ops/` requires a New Era IT Microsoft sign-in (`Voice IN Ops` Entra group). Local uvicorn stays open unless `VOICE_IN_AUTH_ENABLED=true` and Azure AD env vars are set.

Notes:

- the device lane currently uses upload-only WAV input
- the telephony lane reads recent pilot result JSON from `docker/sip-stack/runtime/pilot/results` when available
- if local pilot artifacts are not present, the app will try to read them from the running `voice-ai-sip-freeswitch` container
- live console uses a chat-style board with progress steps (Queue → Heard → Reply → Spoken)
- pilot artifacts are host-mounted at `docker/sip-stack/runtime/pilot` for fast local reads (no slow docker-exec path on every refresh)
- SSE updates every ~150ms when files change, with a 1s poll backup
- mid-turn phases (queued / transcribed / reasoned) show while the pilot worker is still processing
- `9198` opens a session with the SOP greeting as turn 0 as soon as the call answers
- AI calls auto-end after a default max of 6 turns; SOP can transfer to `1000` or end on callback/decline

## Rust rebuild

The project's own code is being rebuilt in Rust as a Cargo workspace (`Cargo.toml`, `crates/`). The Python and Node sources remain until the Rust version is fully proven.

| Crate | What it is | Replaces |
|---|---|---|
| `voice-core` | Config, session files, client packs, xAI REST + realtime clients, call engine, FreeSWITCH event socket, click-to-call | shared logic of `realtime_bridge.js`, `voice_mvp/xai_client.py`, `voice_in_agent/originate.py` |
| `voice-agent` | Realtime Grok voice agent: `serve` (FreeSWITCH audio fork on :3100) and `talk` (PC mic + speakers) | `docker/sip-stack/drachtio-app/realtime_bridge.js` |
| `voice-sessions`, `voice-brand`, `voice-console` | Voice IN console: pages, APIs, live feed, brand voice, Microsoft sign-in | `voice_in_agent/` |
| `voice-cli` (`voice`) | `run`, `check-env`, `record-only`, `text-smoke`, `ingest` | `voice_mvp/`, `scripts/ingest_sbr_audio.py` |

Setup on Windows: install rustup, use the `stable-x86_64-pc-windows-gnu` toolchain plus the WinLibs MinGW toolset (for `dlltool`/`as`). Build output goes to `C:/tmp/voice-ai-target` (see `.cargo/config.toml`) so it is not synced by OneDrive.

```powershell
cargo test --workspace
```

```powershell
cargo run -p voice-agent -- devices
```

```powershell
cargo run -p voice-agent -- talk --client sbr --half-duplex
```

```powershell
.\scripts\start-voice-console.ps1
```

```powershell
.\scripts\start-local-softphone.ps1
```

`talk` needs a valid `XAI_API_KEY` in `.env`; set `VOICE_AI_INPUT_DEVICE` / `VOICE_AI_OUTPUT_DEVICE` to pick the mic and speakers, and drop `--half-duplex` when using a headset. `start-local-softphone.ps1` runs FreeSWITCH and the Rust agent in Docker with no Telair gateway; register MicroSIP as 1000 on `127.0.0.1:5062` and dial 9198 or 9199.

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
