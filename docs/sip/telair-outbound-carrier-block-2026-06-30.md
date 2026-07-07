# Telair Outbound Carrier Block - 2026-06-30

## Summary

Outbound SIP signaling from the local stack to Telair is reaching the carrier and completing digest authentication, but Telair is rejecting the call with:

- `SIP/2.0 503 OUTGOING_CALL_BARRED - REQ006.`

This means the failure is no longer in MicroSIP, FreeSWITCH dialplan normalization, or local Docker routing.

## Environment

- Public IP observed by the stack: `177.225.218.201`
- Telair SIP domain: `sip.telair.net.au`
- Auth ID in use: `76618`
- Primary DID / CLI under test: `0734952900`
- Test destination: `0735434351`

## What Was Proven

1. Local softphone registration to FreeSWITCH works.
2. Local audio path works with the `9196` echo test.
3. FreeSWITCH can send outbound SIP INVITEs to Telair and receive SIP responses.
4. Telair challenges with `407 Proxy Authentication Required`.
5. FreeSWITCH answers that challenge with digest auth for user `76618`.
6. Telair then rejects the authenticated call with `503 OUTGOING_CALL_BARRED - REQ006`.

## Variants Tested

### Destination format

- `+61735434351`
- `0735434351`

Both were rejected by Telair with the same `REQ006` response.

### From / caller identity

- `From: <sip:0734952900@sip.telair.net.au>`
- `From: <sip:76618@sip.telair.net.au>`
- `Remote-Party-ID: <sip:0734952900@sip.telair.net.au>`
- `Remote-Party-ID: <sip:76618@sip.telair.net.au>`

All tested combinations still ended with `503 OUTGOING_CALL_BARRED - REQ006`.

### FreeSWITCH registration mode

- `register=false` and direct authenticated outbound `INVITE`
- `register=true` with explicit `realm=sip.telair.net.au`
- `register=true` with explicit `register-proxy=sip.telair.net.au`

All tested registration variants were rejected by Telair with the same authenticated `503 OUTGOING_CALL_BARRED - REQ006`.

### FreeSWITCH profile / source signaling profile

- gateway on `local_device` profile with SIP source port `5062`
- duplicate test gateway on `drachtio_mrf` profile with contact port `5080`
- temporary direct edge test with FreeSWITCH bound to public host port `5060` after stopping Kamailio

Both profile variants failed registration with the same `503 OUTGOING_CALL_BARRED - REQ006`.
The direct `5060` edge test also failed authenticated outbound calling with the same `503 OUTGOING_CALL_BARRED - REQ006`.

## Key Log Evidence

### Normalized national format test

At approximately `2026-06-30 01:17` local stack time:

```text
INVITE sip:0735434351@sip.telair.net.au SIP/2.0
From: "New Era IT" <sip:0734952900@sip.telair.net.au>
Proxy-Authorization: Digest username="76618", realm="sip.telair.net.au"
SIP/2.0 503 OUTGOING_CALL_BARRED - REQ006.
```

### Auth ID in From test

At approximately `2026-06-30 01:23` local stack time:

```text
INVITE sip:0735434351@sip.telair.net.au SIP/2.0
From: "New Era IT" <sip:76618@sip.telair.net.au>
Remote-Party-ID: "New Era IT" <sip:76618@sip.telair.net.au>
Proxy-Authorization: Digest username="76618", realm="sip.telair.net.au"
SIP/2.0 503 OUTGOING_CALL_BARRED - REQ006.
```

### Direct FreeSWITCH-on-5060 edge test

At approximately `2026-06-30 02:20` local stack time:

```text
INVITE sip:0735434351@sip.telair.net.au SIP/2.0
Via: SIP/2.0/UDP 177.225.218.201
Contact: <sip:gw+telair@177.225.218.201:5060;transport=udp;gw=telair>
From: "New Era IT" <sip:76618@sip.telair.net.au>
Proxy-Authorization: Digest username="76618", realm="sip.telair.net.au"
SIP/2.0 503 OUTGOING_CALL_BARRED - REQ006.
```

## Likely Conclusion

This trunk appears to be carrier-blocked for outbound calling or still requires carrier-side activation, policy change, or IP-based permissioning.

The issue does not appear to be caused by:

- local LAN routing
- Docker networking
- MicroSIP
- FreeSWITCH authentication
- AU number normalization between `+61...` and `0...`

## What To Ask Telair

1. Confirm outbound calling is enabled for trunk/auth user `76618`.
2. Confirm whether they expect authentication-only, source IP whitelisting, or both.
3. Confirm whether public IP `177.225.218.201` must be explicitly whitelisted.
4. Confirm the accepted outbound number format for Australian PSTN calls.
5. Confirm the accepted SIP identity format for the `From` header and caller presentation.
6. Confirm whether direct softphone registration to `sip.telair.net.au` is supported for this service, or only PBX/SBC trunking.
