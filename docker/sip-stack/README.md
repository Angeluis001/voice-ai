# SIP Stack Scaffold

This folder contains the self-hosted Docker scaffold for the SIP-first architecture using `drachtio` as the open-source replacement for Jambonz.

## Intended Topology

`Telair -> Kamailio -> drachtio-server -> drachtio app -> FreeSWITCH`

## Why This Shape

- The workspace MD files lock Kamailio and FreeSWITCH into the long-term wire substrate
- drachtio gives us an open-source application control layer for SIP
- FreeSWITCH remains the media workhorse
- The Node drachtio app becomes the place where AI call logic will later connect

## What This Is Not

- It is not yet a production-hardened SBC deployment
- It is not yet a full AI voice bridge
- It does not yet include MeshCentral

## Local Bring-Up

1. Review `.env.sip.stack`
2. Review `kamailio/kamailio.cfg`
3. Review `drachtio/drachtio.conf.xml.tmpl`
4. Review `drachtio-app/app.js`
5. Validate compose with:

```powershell
docker compose --env-file .env.sip.stack -f docker/sip-stack/docker-compose.yml config
```

## Live Bring-Up

```powershell
docker compose --env-file .env.sip.stack -f docker/sip-stack/docker-compose.yml up -d
```
