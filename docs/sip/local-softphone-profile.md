# Local Softphone Profile

Use this profile to test the current Docker SIP stack on this PC before we attach the AI media bridge.

This is different from the Telair trunk profile:

- this one points to the local Kamailio edge on `localhost:5060`
- it validates the local `Kamailio -> drachtio -> FreeSWITCH` path
- it currently routes test calls to the FreeSWITCH echo target at `9196`

## Recommended App

- `MicroSIP` on Windows

If you prefer Zoiper or Linphone, use the same values.

## Profile Fields

| Setting | Value |
|---------|-------|
| Account label | `Voice AI Local` |
| SIP server / domain | `127.0.0.1` |
| Username / Auth ID | `softphone` |
| Password | leave blank |
| Transport | `UDP` |
| Port | `5060` |
| Display name | `Voice AI Local Test` |
| Register on startup | `Enabled` |

## Why These Values Work

- Kamailio listens on host port `5060`
- Kamailio currently accepts `REGISTER` for local testing
- INVITEs to `9196` are relayed directly to FreeSWITCH as a local health check
- all other INVITEs are relayed to drachtio for the carrier and app-layer path
- FreeSWITCH answers `9196` with a tone plus echo application

## First Call To Place

Dial:

- `9196`

Expected result:

- the softphone should show the call connects
- you should hear your own voice reflected back as echo

## Current Limitations

- Registration is permissive for local testing only
- this path is only the local health check and not the Telair ingress path
- this path does not yet use Grok or the AI audio bridge

## Evidence To Keep

- screenshot of registered state
- screenshot or note showing `9196` connected
- whether echo audio worked, failed, or was one-way
