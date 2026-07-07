# Telair Softphone Profile

Use this as the first local endpoint profile for Phase 1 validation. It is intentionally softphone-first because it minimizes moving parts before SBC setup.

## Recommended App

- `MicroSIP` on Windows

If you prefer another client like Zoiper or Linphone, the same fields apply.

## Profile Fields

| Setting | Value |
|---------|-------|
| Account label | `Telair Test` |
| SIP server / domain | `sip.telair.net.au` |
| Username / Auth ID | `SIP_AUTH_ID` from `.env.sip` |
| Password | `SIP_AUTH_PASSWORD` from `.env.sip` |
| Transport | `UDP` first |
| Port | `5060` |
| Display name | `New Era IT Test` |
| Caller ID expectation | `0NSN` unless Telair changes the trunk to `+E164` |

## Why This Path First

- It proves the carrier path before Docker networking is in play
- It gives visible registration state quickly
- It separates carrier issues from SBC/media stack issues

## First Test Goals

1. Softphone registers successfully
2. Outbound test call can be placed
3. Inbound behavior is understood for the allocated DID range
4. We capture screenshots or logs showing the result

## Evidence To Keep

- Registration status screenshot
- Any SIP error code shown by the client
- Time of test
- Target number used
- Whether audio was one-way, two-way, or failed

## Open Decision

Telair currently presents inbound caller ID in `0NSN` format by default. Before deeper integration, decide whether to leave that as-is or request `+E164` from Telair for cleaner normalization later.
