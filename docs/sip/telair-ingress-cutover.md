# Telair Ingress Cutover

## Goal

Move from the proven local `9196` health check to real Telair inbound SIP without breaking the local test path.

## Current Routing Split

- local softphone test call to `9196`:
  - `MicroSIP -> Kamailio -> FreeSWITCH`
  - expected result: tone plus echo
- Telair inbound or any non-`9196` INVITE:
  - `Telair -> Kamailio -> drachtio-server -> drachtio app -> FreeSWITCH`
  - current target: `B2B_TARGET_URI`

## Telair Values

- auth ID: local secret only
- password: local secret only
- SIP domain: `sip.telair.net.au`
- caller ID format: `0NSN`
- DID range: `0734952900-2999`

## Before First Carrier Test

- confirm the host public IP or edge NAT path that Telair will send traffic to
- confirm UDP `5060` reaches Kamailio from outside the LAN
- confirm UDP RTP `20000-20100` reaches FreeSWITCH from outside the LAN
- confirm Windows firewall and any upstream router rules allow those ports
- pick one DID from the Telair range for the first inbound test

## Recommended First Carrier Proof

1. Keep the local `9196` test working.
2. Ask Telair to send a live inbound call to the chosen DID.
3. Watch `kamailio`, `drachtio-app`, and `freeswitch` logs at the same time.
4. Confirm:
   - the call reaches Kamailio
   - the INVITE is forwarded to drachtio
   - the drachtio app bridges to `B2B_TARGET_URI`
   - the call answers and has two-way audio

## Expected MVP Result

Before the AI bridge is attached, an inbound Telair call should still land on the FreeSWITCH tone-plus-echo behavior through the drachtio bridge.

## Next Step After Carrier Proof

Replace the echo target behind `B2B_TARGET_URI` with the AI media path while keeping `9196` as the permanent local health check.
