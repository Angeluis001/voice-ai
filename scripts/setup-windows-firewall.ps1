[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$rules = @(
  @{Name='Voice AI SIP UDP 5060'; Protocol='UDP'; Port='5060'},
  @{Name='Voice AI SIP TCP 5060'; Protocol='TCP'; Port='5060'},
  @{Name='Voice AI Media UDP 5080'; Protocol='UDP'; Port='5080'},
  @{Name='Voice AI Media TCP 5080'; Protocol='TCP'; Port='5080'},
  @{Name='Voice AI ESL TCP 8021'; Protocol='TCP'; Port='8021'},
  @{Name='Voice AI Drachtio TCP 9022'; Protocol='TCP'; Port='9022'},
  @{Name='Voice AI RTP UDP 20000-20100'; Protocol='UDP'; Port='20000-20100'}
)

foreach ($rule in $rules) {
  Get-NetFirewallRule -DisplayName $rule.Name -ErrorAction SilentlyContinue |
    Remove-NetFirewallRule |
    Out-Null

  New-NetFirewallRule `
    -DisplayName $rule.Name `
    -Direction Inbound `
    -Action Allow `
    -Protocol $rule.Protocol `
    -LocalPort $rule.Port |
    Out-Null
}

Get-NetFirewallRule -DisplayName 'Voice AI*' |
  Get-NetFirewallPortFilter |
  Sort-Object Protocol, LocalPort |
  Format-Table -AutoSize
