# SIP Foundation

This folder holds the SIP foundation artifacts for local Telair validation and the first Dockerized call-flow MVP.

## Files

- `telair-validation-checklist.md` - execution checklist and evidence capture
- `softphone-profile.md` - first recommended endpoint profile for local testing
- `local-softphone-profile.md` - profile for testing the current local Docker SIP stack
- `local-softphone-test.md` - step-by-step local softphone validation checklist

## Local Secret Handling

1. Copy `.env.sip.example` to `.env.sip`
2. Fill in the Telair values locally
3. Do not commit `.env.sip`

## Recommended Proof Order

Start with a softphone registration and outbound test. That gives the fastest confirmation that:

- credentials work
- the trunk accepts the endpoint
- calls can be placed before SBC work begins

After that, move into the local Docker stack:

- Kamailio on the SIP edge
- drachtio-server as the SIP app controller
- a Node `drachtio-srf` app for call control
- FreeSWITCH as media support

The current SIP MVP target is a basic inbound bridge from Kamailio/drachtio into a FreeSWITCH echo endpoint before we attach the AI media path.
