# Docker SIP Stack Runbook

This is the MD-aligned self-hosted path using an open-source replacement for Jambonz.

## Objective

Bootstrap a local Docker stack that follows the locked architecture as closely as possible while replacing Jambonz with drachtio:

- Kamailio at the SIP edge
- drachtio-server as the SIP application control server
- a Node `drachtio-srf` app as the telephony application layer
- FreeSWITCH as media support

MeshCentral remains part of the full MD architecture, but it is intentionally deferred until after SIP, media, and AI call handling are proven.

## Why drachtio

The drachtio docs describe it as a Node.js framework for SIP applications that works in concert with a `drachtio-server`, and note that the easiest way to get started is with a Docker image. The drachtio Docker image is published as `drachtio/drachtio-server`, and there is also a slim FreeSWITCH image built specifically for `drachtio-fsmrf`.

## Files

- `.env.sip.stack.example`
- `docker/sip-stack/docker-compose.yml`
- `docker/sip-stack/kamailio/kamailio.cfg`
- `docker/sip-stack/drachtio/drachtio.conf.xml.tmpl`
- `docker/sip-stack/drachtio-app/`
- `docker/sip-stack/freeswitch/conf/vars.xml`

## Recommended Sequence

1. Review `.env.sip.stack`
2. Confirm Docker Desktop networking and host port availability
3. Run `docker compose --env-file .env.sip.stack -f docker/sip-stack/docker-compose.yml config`
4. Pull images and start the stack
5. Confirm `drachtio-app` connects to `drachtio-server`
6. Confirm Kamailio forwards SIP traffic into drachtio
7. Verify the `drachtio-app` can bridge an INVITE to the FreeSWITCH echo target
8. Only then move into AI call handling and media bridging

## First Live Validation Goals

- Kamailio listens on the host SIP port
- drachtio-server listens for SIP on `5060` and app control on `9022`
- the local Node app connects successfully to drachtio
- FreeSWITCH ESL is reachable internally
- carrier traffic can be forwarded from Kamailio to drachtio
- an inbound INVITE can be bridged to `B2B_TARGET_URI`, which defaults to the FreeSWITCH echo test at `sip:9196@freeswitch:5080`

## Current Edge Routing

- `REGISTER` is permissive for local softphone testing only
- `INVITE` to `9196` is routed directly to FreeSWITCH as the local media health check
- all other `INVITE` requests are routed from Kamailio to `drachtio-server`
- the drachtio app bridges those calls to `B2B_TARGET_URI`

This split lets us keep a fast local proof path while preparing the real Telair ingress path through the SIP application layer.

## Known Gaps

- Telair-specific registration or ingress policy still needs carrier-side proof
- RTP exposure and NAT policy need live validation
- The current drachtio app is a SIP bridge MVP, not the final AI media bridge
