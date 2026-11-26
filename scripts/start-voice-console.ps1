# Starts the Rust Voice IN console (replaces `uvicorn voice_in_agent.app:app`).
param(
    [string]$HostAddress = "0.0.0.0",
    [int]$Port = 8000,
    [switch]$Release
)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
Write-Host "Voice IN console (Rust): http://127.0.0.1:$Port  (local start is unauthenticated unless AZURE_AD_* is set)"
$cargoArgs = @("run", "-p", "voice-console")
if ($Release) { $cargoArgs += "--release" }
& cargo @cargoArgs -- --host $HostAddress --port $Port
