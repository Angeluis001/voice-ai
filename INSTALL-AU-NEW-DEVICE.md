# Voice AI New Device Install Guide (AU)

This guide is for installing this repository on a new Windows device in Australia, with Telair as the SIP carrier and `sip.telair.net.au` as the carrier domain.

It covers two layers:

1. the desktop Voice AI MVP
2. the SIP-first Telair and Docker stack path used for AU telephony validation

## Scope

Use this guide when you need a fresh machine to be able to:

- run the local Voice AI MVP
- store Telair credentials locally and safely
- register a softphone against Telair
- bring up the Docker SIP stack
- prepare AU inbound testing and know the current carrier caveats

## What This Repo Contains

- `voice_mvp/` for the local desktop voice flow
- `.env.example` for xAI settings
- `.env.sip.example` for direct Telair softphone testing
- `.env.sip.stack.example` for the Docker SIP stack
- `docker/sip-stack/` for Kamailio, drachtio, FreeSWITCH, and the Node bridge app
- `docs/sip/` for validation, softphone, ingress, and network notes

## Recommended Install Order

1. install Windows prerequisites
2. clone the repo
3. configure `.env` and verify the desktop MVP
4. configure `.env.sip` and prove Telair registration with a softphone
5. configure `.env.sip.stack` and bring up the Docker stack
6. open firewall and router/NAT rules if inbound AU testing is required

## Prerequisites

Install these on the new Windows device:

- `Git`
- `Python 3.11+`
- `Docker Desktop`
- `Node.js 20+`
- `MicroSIP` for first SIP proof
- a headset or microphone/speaker device for call testing

Recommended checks:

```powershell
git --version
python --version
node --version
docker --version
```

## Clone The Repo

```powershell
git clone https://github.com/Angeluis001/voice-ai.git
cd voice-ai
```

## Part 1: Desktop Voice AI MVP

### 1. Create the local Python environment

```powershell
python -m venv .venv
.\.venv\Scripts\Activate.ps1
pip install -r requirements.txt
```

### 2. Create `.env`

Copy `.env.example` to `.env` and fill these values:

```env
XAI_API_KEY=replace-me-locally
XAI_BASE_URL=https://api.x.ai/v1
VOICE_MVP_LANGUAGE=en
VOICE_MVP_STT_KEYTERM=
VOICE_MVP_GROK_MODEL=grok-4.3
VOICE_MVP_TTS_VOICE=eve
VOICE_MVP_RECORD_SECONDS=5
VOICE_MVP_SAMPLE_RATE=16000
```

Minimum required value:

- `XAI_API_KEY`

### 3. Run the MVP

```powershell
python -m voice_mvp run
```

Expected result:

- microphone audio is captured
- STT is sent to xAI
- Grok returns a structured response
- TTS returns WAV output
- audio plays locally through Windows

## Part 2: AU Telair Softphone Proof

This is the fastest way to separate carrier issues from Docker and SBC issues.

### 1. Create `.env.sip`

Copy `.env.sip.example` to `.env.sip` and fill it with your live Telair details:

```env
SIP_PROVIDER=telair
SIP_AUTH_ID=replace-me-locally
SIP_AUTH_PASSWORD=replace-me-locally
SIP_DOMAIN=sip.telair.net.au
SIP_PORT=5060
SIP_TRANSPORT=udp
SIP_DID_RANGE=0734952900-2999
SIP_INBOUND_CALLER_ID_FORMAT=0NSN
SIP_REQUEST_E164=false
SIP_TEST_TARGET_NUMBER=
SIP_TEST_DIRECTION=outbound
SIP_SOFTPHONE=microsip
SIP_NOTES=
```

AU-specific assumptions already captured in this repo:

- carrier domain is `sip.telair.net.au`
- first transport should be `UDP`
- default SIP port is `5060`
- inbound caller ID is expected as `0NSN`
- DID ranges may be delivered in Australian national format, not `+61`

### 2. Configure MicroSIP

Use the values documented in `docs/sip/softphone-profile.md`:

| Setting | Value |
|---------|-------|
| Account label | `Telair Test` |
| SIP server / domain | `sip.telair.net.au` |
| Username / Auth ID | `SIP_AUTH_ID` from `.env.sip` |
| Password | `SIP_AUTH_PASSWORD` from `.env.sip` |
| Transport | `UDP` |
| Port | `5060` |
| Display name | `New Era IT Test` |
| Caller ID expectation | `0NSN` |

### 3. First validation target

The first proof should be:

1. registration success
2. outbound test call attempt
3. inbound DID behavior check

Use `docs/sip/telair-validation-checklist.md` as the evidence checklist.

### 4. Evidence to keep

Capture:

- registration screenshot
- exact SIP error text or code
- target number used
- test timestamp
- whether audio was two-way, one-way, or absent

## Important AU Carrier Caveat

As of the existing proof in this repo, outbound authenticated SIP reached Telair successfully but Telair rejected the outbound call with:

- `503 OUTGOING_CALL_BARRED - REQ006`

That means:

- local auth was already proven
- local Docker routing was not the root cause
- AU number format testing between `+61...` and `0...` was not the root cause
- Telair likely still needs outbound enablement, policy change, or source IP whitelisting

See:

- `docs/sip/telair-outbound-carrier-block-2026-06-30.md`

Before assuming the new device is misconfigured, compare your results against that note.

## Part 3: Docker SIP Stack

This repo uses:

- `Kamailio` as SIP edge
- `drachtio-server` as SIP control server
- a Node `drachtio-srf` app as the bridge layer
- `FreeSWITCH` as the media engine

### 1. Create `.env.sip.stack`

Copy `.env.sip.stack.example` to `.env.sip.stack`.

Minimum values to replace locally:

```env
TELAIR_AUTH_ID=replace-me
TELAIR_AUTH_PASSWORD=replace-me
DRACHTIO_SECRET=replace-me
FREESWITCH_ESL_PASSWORD=replace-me
VOICE_APP_WEBHOOK_SECRET=replace-me
FREESWITCH_EXTERNAL_IP=replace-with-public-ip
MICROSIP_EXTENSION_PASSWORD=replace-me
```

Important AU values that should normally stay as-is unless Telair tells you otherwise:

```env
TELAIR_SIP_DOMAIN=sip.telair.net.au
TELAIR_TRANSPORT=udp
TELAIR_LISTEN_PORT=5060
TELAIR_INBOUND_CALLER_ID_FORMAT=0NSN
TELAIR_DID_RANGE=0734952900-2999
HOST_SIP_PORT=5060
HOST_FREESWITCH_SIP_PORT=5080
HOST_FREESWITCH_DEVICE_SIP_PORT=5062
HOST_FREESWITCH_RTP_START=20000
HOST_FREESWITCH_RTP_END=20100
B2B_TARGET_URI=sip:9196@freeswitch:5080
```

### 2. Validate Docker config

```powershell
docker compose --env-file .env.sip.stack -f docker/sip-stack/docker-compose.yml config
```

### 3. Start the stack

```powershell
docker compose --env-file .env.sip.stack -f docker/sip-stack/docker-compose.yml up -d
```

### 4. Verify the stack

Look for:

- Kamailio listening on host SIP ports
- drachtio listening for SIP on `5060`
- drachtio admin on `9022`
- FreeSWITCH SIP on `5080` and device SIP on `5062`
- RTP range `20000-20100`
- the Node app successfully connected to `drachtio-server`

The local health-check route is:

- dial `9196` to hit the FreeSWITCH echo target

See:

- `docs/sip/docker-stack-runbook.md`
- `docs/sip/local-softphone-profile.md`
- `docs/sip/local-softphone-test.md`

## Part 4: AU Inbound Readiness

If this new device must receive live inbound AU Telair calls, you must also prepare Windows Firewall and router/NAT rules.

### Required inbound path

Minimum external forwards:

- `UDP 5060`
- `UDP 5062`
- `UDP 20000-20100`

Recommended wider observability path:

- `TCP 5060`
- `TCP 5062`
- `UDP 5080`
- `TCP 5080`
- `TCP 8021`
- `TCP 9022`

### Windows firewall

Run this from an elevated PowerShell window:

```powershell
powershell -ExecutionPolicy Bypass -File ".\scripts\setup-windows-firewall.ps1"
```

### Router/NAT guidance

Forward the public-side ports to the Windows machine that is hosting the stack.

Do not copy the old host values from previous tests blindly. On a new AU device, these must match the new machine:

- LAN IP
- public IP
- router/NAT target
- `FREESWITCH_EXTERNAL_IP`

See:

- `docs/sip/telair-edge-readiness.md`
- `docs/sip/telair-ingress-cutover.md`

## Secrets And Safety

Never commit these local files:

- `.env`
- `.env.sip`
- `.env.sip.stack`

This repo already ignores them, but keep the rule operationally too:

- store secrets only on the device
- avoid screenshots that expose passwords
- do not paste tokens or SIP passwords into tracked docs

## Recommended Fresh-Device Validation Sequence

Run this in order:

1. `python -m voice_mvp run`
2. register MicroSIP directly to `sip.telair.net.au`
3. attempt one outbound AU test call
4. capture any SIP code, especially `REQ006`
5. bring up Docker with `.env.sip.stack`
6. verify local echo route `9196`
7. only then move to inbound Telair cutover

## Quick Troubleshooting

If the desktop MVP fails:

- verify `.venv` is active
- verify `XAI_API_KEY`
- verify microphone permissions in Windows

If Telair registration fails:

- verify `SIP_AUTH_ID`
- verify `SIP_AUTH_PASSWORD`
- verify `sip.telair.net.au`
- keep transport on `UDP` first

If outbound calls fail with `REQ006`:

- treat that as a likely carrier-side block, not a first assumption of local misconfiguration

If calls connect but audio fails:

- check Windows firewall
- check router/NAT RTP forwarding
- check `FREESWITCH_EXTERNAL_IP`
- note whether audio is one-way or both-way failure

## Source Docs

Primary references already in this repo:

- `README.md`
- `docs/sip/README.md`
- `docs/sip/softphone-profile.md`
- `docs/sip/telair-validation-checklist.md`
- `docs/sip/docker-stack-runbook.md`
- `docs/sip/telair-edge-readiness.md`
- `docs/sip/telair-ingress-cutover.md`
- `docs/sip/telair-outbound-carrier-block-2026-06-30.md`

## Suggested Next Improvement

After this guide is used once on a second AU device, update it with:

- exact install versions that worked
- the actual AU public IP and LAN pattern for that site
- whether Telair enabled outbound service for the trunk
- whether inbound caller ID remained `0NSN` or moved to `+E164`
