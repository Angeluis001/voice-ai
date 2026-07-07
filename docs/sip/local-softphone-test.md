# Local Softphone Test

## Goal

Prove that a real SIP softphone on this Windows PC can place a call through the local stack and reach the FreeSWITCH echo target.

## Preconditions

- Docker stack is running
- `voice-ai-sip-kamailio`, `voice-ai-sip-drachtio-server`, `voice-ai-sip-drachtio-app`, and `voice-ai-sip-freeswitch` are up
- a softphone is installed

## Steps

1. Create a softphone account using `docs/sip/local-softphone-profile.md`
2. Confirm the client shows a registered or online state
3. Place a call to `9196`
4. Speak into the microphone for a few seconds
5. Confirm whether you hear the echo response

## Success Criteria

- the softphone can register locally
- the call to `9196` connects
- audio returns as echo

## If It Fails

If registration fails:

- confirm the client points to `127.0.0.1`
- confirm transport is `UDP`
- confirm port is `5060`
- capture the exact client error message

If the call does not connect:

- capture the SIP code shown by the client
- capture the dialed target
- keep the call time so we can match logs

If audio fails:

- note whether the issue is one-way or no audio
- keep the exact test time for RTP log matching
