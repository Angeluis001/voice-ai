# Starts the local softphone test stack: FreeSWITCH + the Rust voice agent. No carrier.
#
# 1. Writes freeswitch/conf/vars.local.xml from vars.xml with this PC's address as the
#    advertised softphone audio IP (the file holds passwords and is not committed).
# 2. Builds the Rust agent image and starts `freeswitch` and `drachtio-app` only.
#
# Then register MicroSIP: server 127.0.0.1:5062, domain voiceai.local, user 1000.
# Dial 9198 (AI enquiries) or 9199 / 9199fees / 9199transfer (roleplay).
param(
    # Address softphones on this PC or LAN can reach. Default: first LAN IPv4.
    [string]$AdvertisedIp = "",
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$stack = Join-Path $repo "docker\sip-stack"
$envFile = Join-Path $repo ".env.sip.stack"
if (-not (Test-Path $envFile)) { throw "Missing $envFile (copy .env.sip.stack.example and fill it in)." }

if (-not $AdvertisedIp) {
    $AdvertisedIp = Get-NetIPAddress -AddressFamily IPv4 |
        Where-Object { $_.IPAddress -match '^(192\.168|10\.)' -and $_.InterfaceAlias -notmatch 'vEthernet|VMware|VirtualBox|WSL|Loopback' -and $_.AddressState -eq 'Preferred' } |
        Sort-Object InterfaceMetric | Select-Object -First 1 -ExpandProperty IPAddress
}
if (-not $AdvertisedIp) { throw "Could not detect a LAN address. Pass -AdvertisedIp." }

$vars = Get-Content (Join-Path $stack "freeswitch\conf\vars.xml") -Raw
$vars = $vars -replace 'data="device_advertised_ip=[^"]*"', "data=`"device_advertised_ip=$AdvertisedIp`""
[System.IO.File]::WriteAllText((Join-Path $stack "freeswitch\conf\vars.local.xml"), $vars, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "Softphone audio advertised on $AdvertisedIp"

# Prefer the xAI key from the repo's .env (the one the local tools use) over the one in
# .env.sip.stack. A variable set in the shell wins over the compose env file.
$dotenv = Join-Path $repo ".env"
if (Test-Path $dotenv) {
    $line = Get-Content $dotenv | Where-Object { $_ -match '^XAI_API_KEY=' } | Select-Object -First 1
    if ($line) { $env:XAI_API_KEY = ($line -replace '^XAI_API_KEY=', '').Trim().Trim('"') }
}

Push-Location $stack
# Docker writes progress to stderr; that must not be treated as a failure.
$ErrorActionPreference = "Continue"
try {
    $compose = @("compose", "--env-file", $envFile, "-f", "docker-compose.yml", "-f", "docker-compose.rust.yml")
    $up = @("up", "-d")
    if (-not $NoBuild) { $up += "--build" }
    & docker @compose @up freeswitch drachtio-app
    if ($LASTEXITCODE -ne 0) { throw "docker compose failed." }
    & docker @compose ps
}
finally { Pop-Location }

Write-Host ""
Write-Host "MicroSIP: server 127.0.0.1:5062, domain voiceai.local, user 1000 (password: MICROSIP_EXTENSION_PASSWORD in vars.xml)."
Write-Host "Dial 9198 for the AI, 9199 for roleplay. Agent logs: docker logs -f voice-ai-sip-drachtio-app"
