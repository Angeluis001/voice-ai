# Rust Architecture

**Last updated:** 2026-11-23

The project's own code is being rebuilt in Rust as a Cargo workspace (`Cargo.toml` and `crates/`). Rust is the team's default language. This page describes what the Rust code does today, as read from the code. For the older Python/Node design, see `docs/PROJECT-FULL-SUMMARY.md`.

## Status: what is live, what is Rust

The Python and Node sources stay in the repo until the Rust version is proven. Until then:

| Area | Live path today | Rust replacement | Where Rust is used now |
|------|-----------------|------------------|------------------------|
| Realtime AI bridge (FreeSWITCH audio fork to xAI) | `docker/sip-stack/drachtio-app/realtime_bridge.js` (Node) | `voice-agent serve` | Local softphone stack (`docker-compose.rust.yml`) |
| Voice IN console | `voice_in_agent/` (Python, FastAPI, uvicorn) | `voice-console` | `scripts/start-voice-console.ps1` |
| Desktop CLI (mic, STT, Grok, TTS) | `voice_mvp/` (Python) | `voice` (`voice-cli`) | Local runs with `cargo run` |
| SBR audio ingest | `scripts/ingest_sbr_audio.py` | `voice ingest` | Local runs with `cargo run` |

The Azure/HQ overlays (`docker-compose.azure.yml`, `docker-compose.console.yml`) in the repo still use the Node app and the Python console (`docker-compose.console.yml` runs `uvicorn voice_in_agent.app:app`). The Rust overlay is only for the local softphone stack.

## Crates

| Crate | What it does |
|-------|--------------|
| `voice-core` | Shared library. Environment config, session files, client packs, xAI REST and realtime clients, the call engine, the FreeSWITCH event socket (ESL), click-to-call (originate), audio helpers (WAV, resample, mixer), intent and slot handling, post-call analysis. |
| `voice-agent` | Binary `voice-agent`. Realtime Grok voice agent with three commands: `serve`, `talk`, `devices`. |
| `voice-sessions` | Session service for the console: session models, prompts, summaries and analysis. |
| `voice-brand` | Brand voice store and logic used by the console's Brand page. |
| `voice-console` | Binary `voice-console`. The Voice IN web server (pages, APIs, live feed, Microsoft sign-in). |
| `voice-cli` | Binary `voice`. Desktop CLI: `run`, `check-env`, `record-only`, `text-smoke`, `ingest`. |

## Commands

Run from the repo root. Build output goes to `C:/tmp/voice-ai-target` (see `.cargo/config.toml`) so OneDrive does not sync it.

### Tests

```powershell
cargo test --workspace
```

### voice-agent

```powershell
cargo run -p voice-agent -- serve
cargo run -p voice-agent -- devices
cargo run -p voice-agent -- talk --client sbr --half-duplex
```

| Command | What it does |
|---------|--------------|
| `serve` | Answers softphone calls. Listens on port 3100 for FreeSWITCH `uuid_audio_fork` connections. |
| `talk` | Talk to the agent through this PC's microphone and speakers. |
| `devices` | Lists the microphones and speakers on this PC. |

`talk` options:

| Option | Meaning |
|--------|---------|
| `--client <id>` | Client pack to use (folder name under the clients directory). |
| `--roleplay` | Training mode: the AI plays the customer, you play the agent. |
| `--scenario <key>` | Roleplay scenario, for example `fees`, `unsupported`, `transfer`, `angry`. |
| `--half-duplex` | Mutes the microphone while the AI speaks. Use this without a headset. |
| `--caller-wav <file>` | Plays a WAV file as the caller instead of the microphone. Repeat for several utterances. |
| `--seconds <n>` | Hangs up automatically after n seconds. |

When a `talk` call ends it prints the session file path, the recording path, the status and the summary.

### voice (voice-cli)

```powershell
cargo run -p voice-cli -- check-env
cargo run -p voice-cli -- run
cargo run -p voice-cli -- record-only --seconds 5
cargo run -p voice-cli -- text-smoke --text "Hello" --speak
cargo run -p voice-cli -- ingest --source <folder> --out artifacts/sbr-ingest --client sbr
```

| Command | What it does |
|---------|--------------|
| `run` | Records, transcribes, reasons, synthesizes and plays one interaction. |
| `check-env` | Prints the loaded configuration (the API key is shown as redacted). |
| `record-only` | Records a WAV file without calling xAI. `--seconds` overrides the length. |
| `text-smoke` | Tests Grok (and optionally TTS with `--speak`) without a microphone. |
| `ingest` | Turns a folder of call recordings (wav, mp3, m4a, ogg, flac) into console sessions with summaries. Defaults: `--out artifacts/sbr-ingest`, `--client sbr`. |

`voice` needs `XAI_API_KEY`; it stops with an error if it is missing.

### voice-console

```powershell
.\scripts\start-voice-console.ps1
.\scripts\start-voice-console.ps1 -Port 8000 -HostAddress 0.0.0.0 -Release
```

The script runs `cargo run -p voice-console` (add `-Release` for a release build) with `--host` and `--port`. Defaults: host `0.0.0.0`, port `8000`. The host and port can also come from `VOICE_IN_HOST` and `VOICE_IN_PORT`. A local start has no sign-in unless the Azure AD variables are set.

### Local softphone stack

```powershell
.\scripts\start-local-softphone.ps1
```

The script:

1. Needs `.env.sip.stack` (copy `.env.sip.stack.example` and fill it in).
2. Writes `docker/sip-stack/freeswitch/conf/vars.local.xml` with this PC's LAN address as the advertised audio IP (it holds passwords and is not committed). Use `-AdvertisedIp` to set the address yourself.
3. Takes `XAI_API_KEY` from the repo `.env` if present.
4. Runs `docker compose` with `docker-compose.yml` plus `docker-compose.rust.yml` and starts only `freeswitch` and `drachtio-app`. Use `-NoBuild` to skip the image build.

The overlay replaces the Node realtime bridge with `voice-agent serve`, built by `rust/Dockerfile.agent` into the image `voice-ai-agent:local`. The service keeps the name `drachtio-app`, so the dialplan URL `ws://drachtio-app:3100/media` does not change. It loads a FreeSWITCH profile with no Telair gateway, so this PC never registers the trunk. Kamailio and drachtio-server stay stopped.

Register MicroSIP with server `127.0.0.1:5062`, domain `voiceai.local`, user `1000`. Dial `9198` for the AI or `9199` for roleplay (the script also lists `9199fees` and `9199transfer`). Agent logs: `docker logs -f voice-ai-sip-drachtio-app`.

## Ports and routes

| Binary | Port | Routes |
|--------|------|--------|
| `voice-agent serve` | `3100` (`VOICE_AI_REALTIME_PORT`) | `/health`, `/media` (WebSocket from the FreeSWITCH audio fork) |
| `voice-console` | `8000` (`VOICE_IN_PORT`) | Pages: `/`, `/console`, `/assist`, `/record`, `/capture`, `/train`, `/create`, `/brand`, `/summary/{session_id}`, `/linphone/qr`. Health: `/healthz`. Sign-in: `/auth/login`, `/auth/callback`, `/auth/logout`. APIs: `/api/telephony/*` (recent, sessions, events, originate, recording, summary), `/api/brand/*`, `/api/device/upload`, `/api/linphone/provision/{extension}`. |
| FreeSWITCH event socket (used by the agent) | `8021` default | Transfers and playout clearing |

`/media` query parameters read by the agent: `call_id` (or `conversation_id`), `uuid`, `target` (default `9198`), `mode` (default `realtime`), `client_id` (or `client`), `scenario`. A call is roleplay when `mode=roleplay` or the target is `9199`. Transfers need a channel `uuid`. The agent clears playout and transfers calls through the FreeSWITCH event socket, never by sending control JSON down the fork socket.

The audio fork carries raw L16 mono PCM. The sample rate for softphone calls is 8000 Hz by default.

## Environment variables

The Rust binaries read a `.env` file from the working directory or any parent (a missing file is fine). Variable names are the same as the Python version, so the same `.env` works. See `.env.example` for the main ones. Never put real values in docs.

### xAI

| Variable | Default |
|----------|---------|
| `XAI_API_KEY` | none (required by `voice`) |
| `XAI_BASE_URL` | `https://api.x.ai/v1` |
| `XAI_REALTIME_URL` | `wss://api.x.ai/v1/realtime?model=grok-voice-think-fast-1.0` |
| `VOICE_MVP_GROK_MODEL` | `grok-4.3` |
| `VOICE_MVP_TTS_VOICE` | `eve` |
| `VOICE_MVP_LANGUAGE` | `en` |
| `VOICE_MVP_STT_KEYTERM` | none |

### Voice agent

| Variable | Default |
|----------|---------|
| `VOICE_AI_REALTIME_PORT` | `3100` |
| `VOICE_AI_REALTIME_SAMPLE_RATE` | `8000` (softphone calls) |
| `VOICE_AI_LOCAL_SAMPLE_RATE` | `24000` (`talk` mode) |
| `VOICE_AI_INPUT_DEVICE`, `VOICE_AI_OUTPUT_DEVICE` | system default (part of the device name is enough) |
| `VOICE_AI_VAD_THRESHOLD` | `0.55` |
| `VOICE_AI_VAD_SILENCE_MS` | `480` |
| `VOICE_AI_VAD_PREFIX_MS` | `200` |
| `VOICE_AI_TRANSCRIBE_MODEL` | `grok-transcribe` |
| `VOICE_AI_LANGUAGE_HINT` | `en` |
| `VOICE_AI_CLIENTS_DIR` | `docker/sip-stack/clients` |
| `VOICE_AI_DEFAULT_CLIENT_ID` | `default` |
| `VOICE_AI_SOP_GREETING` | none |
| `VOICE_AI_PILOT_RESULT_DIR` | `docker/sip-stack/runtime/pilot/results` |
| `VOICE_AI_PILOT_MEDIA_DIR` | `docker/sip-stack/runtime/pilot/media` |

### FreeSWITCH event socket

| Variable | Default |
|----------|---------|
| `FREESWITCH_HOST` | `freeswitch` (agent) |
| `FREESWITCH_PORT` | `8021` (agent) |
| `FREESWITCH_SECRET` or `FREESWITCH_ESL_PASSWORD` | none |
| `FREESWITCH_ESL_HOST` | `172.17.0.1` (click-to-call in the console) |
| `FREESWITCH_ESL_PORT` | `8021` (click-to-call in the console) |

### Click-to-call (console)

`VOICE_DIAL_RULES_PATH`, `VOICE_OPERATOR_ENDPOINT`, `VOICE_OUTBOUND_CLI`, `VOICE_OUTBOUND_CLI_NAME`, `VOICE_OUTBOUND_GATEWAY`. All optional.

### Console

| Variable | Default |
|----------|---------|
| `VOICE_IN_HOST` / `VOICE_IN_PORT` | `0.0.0.0` / `8000` |
| `VOICE_IN_ARTIFACTS_DIR` | `artifacts/voice_in_agent` (also read by `voice` as a fallback for its artifacts folder) |
| `VOICE_IN_BRAND_DIR` | `<artifacts dir>/brand` |
| `VOICE_IN_TELEPHONY_RESULTS_DIR` | `docker/sip-stack/runtime/pilot/results` |
| `VOICE_IN_TELEPHONY_MEDIA_DIR` | `docker/sip-stack/runtime/pilot/media` |
| `VOICE_IN_ACTIVE_WINDOW_SECONDS` | `180` (a call counts as live if updated within this time) |
| `VOICE_IN_BASE_PATH` | empty (set to `/ops` behind a reverse proxy) |
| `VOICE_IN_AUTH_ENABLED` | sign-in is on only if the Azure AD variables are set, or this is `true` |
| `VOICE_IN_SESSION_SECRET`, `VOICE_IN_SESSION_SECURE` | used with Microsoft sign-in |
| `AZURE_AD_TENANT_ID`, `AZURE_AD_CLIENT_ID`, `AZURE_AD_CLIENT_SECRET`, `AZURE_AD_REDIRECT_URI`, `AZURE_AD_AUTHORITY` | Microsoft sign-in |
| `VOICE_LINPHONE_1001_PASSWORD` | Linphone provisioning for extension 1001 is off unless this is set |
| `VOICE_LINPHONE_DOMAIN` | `voiceai.local` |
| `VOICE_LINPHONE_PROXY_HOST` / `VOICE_LINPHONE_PROXY_PORT` | `100.76.98.45` / `5062` |

### Desktop CLI (`voice`)

| Variable | Default |
|----------|---------|
| `VOICE_MVP_RECORD_SECONDS` | `5` |
| `VOICE_MVP_SAMPLE_RATE` | `16000` |
| `VOICE_MVP_ARTIFACTS_DIR` (or `VOICE_IN_ARTIFACTS_DIR`) | `artifacts` |
| `VOICE_MVP_LOGS_DIR` | `logs` |

Logging for all Rust binaries uses `RUST_LOG` (default `info`).

## Docker bits

- `rust/Dockerfile.agent` builds only `voice-agent` (`cargo build --release -p voice-agent`) and runs `voice-agent serve`, exposing port 3100. Build context is the repo root. It overrides the Windows build path from `.cargo/config.toml`.
- `.dockerignore` keeps the build context small: only Cargo files and `crates/` are needed.
- `docker/sip-stack/docker-compose.rust.yml` is the local softphone overlay described above.

## Windows setup

Install rustup and use the `stable-x86_64-pc-windows-gnu` toolchain plus the WinLibs MinGW toolset (for `dlltool` and `as`). See the "Rust rebuild" section in `README.md`.

## Not covered by Rust yet

Nothing in `crates/` implements the drachtio-srf SIP application (`docker/sip-stack/drachtio-app/app.js`), the turn-based pilot routes, or the Docker image for the console. These stay on Node/Python.