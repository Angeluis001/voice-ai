# Telair SIP Validation Checklist

Phase 1 goal: prove the carrier path locally before SBC and AI bridge work.

## Preparation

- [ ] Create local `.env.sip` from `.env.sip.example`
- [ ] Confirm `SIP_AUTH_ID`, `SIP_AUTH_PASSWORD`, and `SIP_DOMAIN` are filled locally
- [ ] Confirm the softphone is installed on this PC
- [ ] Confirm Windows firewall or endpoint protection is not silently blocking the softphone
- [ ] Decide whether the first proof is outbound, inbound, or registration-only

## Recommended First Run

Use:

- endpoint type: softphone
- transport: `UDP`
- target proof: outbound call first

Reason:

- fastest way to confirm auth plus call path
- easiest to separate carrier issues from future Docker issues

## Configuration Checks

- [ ] SIP server/domain is `sip.telair.net.au`
- [ ] Username/auth ID matches the local secret value
- [ ] Password matches the local secret value
- [ ] Port is `5060`
- [ ] Transport is `UDP`
- [ ] Caller ID assumptions are documented as `0NSN`

## Registration Validation

- [ ] Softphone shows registered or equivalent success state
- [ ] If registration fails, capture the exact client error text or code
- [ ] If possible, save or export any SIP client logs

Success evidence:

- screenshot of registered state
- timestamp of successful registration

## Outbound Call Validation

- [ ] Place a test call to the agreed target number
- [ ] Confirm whether ringing occurs
- [ ] Confirm whether call connects
- [ ] Confirm whether audio is two-way, one-way, or absent
- [ ] Note presented caller ID if visible

Success evidence:

- target number used
- call start time
- connection outcome
- audio outcome

## Inbound Validation

- [ ] Choose one DID in the allocated range for inbound testing
- [ ] Confirm whether the softphone rings
- [ ] Confirm whether call can be answered
- [ ] Confirm audio path result
- [ ] Record the presented inbound caller ID format

Success evidence:

- DID used
- whether the endpoint rang
- answer result
- audio result

## Failure Triage

If registration fails:

- verify auth ID
- verify password
- verify domain
- try `UDP` before changing transports
- capture the SIP code or client message before changing multiple variables

If call connects but audio fails:

- note whether it is one-way or both-way failure
- capture local network context
- defer deep RTP debugging until Phase 2 if the carrier path itself is already proven

## Exit Criteria For Phase 1

- [ ] We have at least one real proof of SIP readiness: successful registration or a successful test call
- [ ] Telair secrets are stored locally and not in tracked files
- [ ] We know whether `0NSN` is acceptable or whether to request `+E164`
- [ ] We have enough evidence to move into Docker SBC/media work
