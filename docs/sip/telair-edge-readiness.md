# Telair Edge Readiness

## Current Host Values

- LAN IP: `192.168.1.12`
- Gateway: `192.168.1.1`
- Observed public IP on 2026-06-25: `177.225.218.201`

## Confirmed Listening Ports On This PC

- Kamailio SIP edge:
  - `UDP 5060`
  - `TCP 5060`
- FreeSWITCH media SIP:
  - `UDP 5080`
  - `TCP 5080`
- FreeSWITCH device SIP:
  - `UDP 5062`
  - `TCP 5062`
- FreeSWITCH ESL:
  - `TCP 8021`
- drachtio admin:
  - `TCP 9022`
- RTP media:
  - `UDP 20000-20100`

## What Must Be Open For First Telair Inbound Test

Minimum external path:

- router/NAT public side `UDP 5060` -> `192.168.1.12:5060`
- router/NAT public side `UDP 5062` -> `192.168.1.12:5062`
- router/NAT public side `UDP 20000-20100` -> `192.168.1.12:20000-20100`

Recommended full path for observability and future work:

- `TCP 5060` -> `192.168.1.12:5060`
- `TCP 5062` -> `192.168.1.12:5062`
- `UDP 5080` -> `192.168.1.12:5080`
- `TCP 5080` -> `192.168.1.12:5080`
- `TCP 8021` -> `192.168.1.12:8021`
- `TCP 9022` -> `192.168.1.12:9022`

## Windows Firewall

Use the admin PowerShell script:

- [setup-windows-firewall.ps1](C:/Users/angel/OneDrive%20-%20New%20Era%20IT/Voice%20AI/scripts/setup-windows-firewall.ps1)

Run it in an elevated PowerShell window:

```powershell
powershell -ExecutionPolicy Bypass -File ".\scripts\setup-windows-firewall.ps1"
```

## Router/NAT Setup

Create these port forwards on the router:

1. `UDP 5060` -> `192.168.1.12:5060`
2. `UDP 5062` -> `192.168.1.12:5062`
3. `UDP 20000-20100` -> `192.168.1.12:20000-20100`

If the router only supports one port at a time for ranges, add the whole RTP range exactly as `20000-20100`.

## Telair Carrier Test Target

After the NAT rules are active:

1. pick one DID from `0734952900-2999`
2. ask Telair to send an inbound test call
3. keep the local health check `9196` unchanged
4. watch `kamailio`, `drachtio-app`, and `freeswitch` logs during the call

## Important Limitation

This Codex session could not modify Windows Firewall directly because the host returned `Access is denied` for firewall commands, which indicates the current shell is not running with real administrator privileges.
