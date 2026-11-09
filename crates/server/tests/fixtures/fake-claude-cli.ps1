$ErrorActionPreference = 'Stop'

if ($env:ANY_CONVERTER_OPENED_URL -and $args.Count -eq 1 -and $args[0].StartsWith('https://')) {
    Set-Content -LiteralPath $env:ANY_CONVERTER_OPENED_URL -Value $args[0] -NoNewline
    exit 0
}

if ($args.Count -ge 3 -and $args[0] -eq 'auth' -and $args[1] -eq 'status' -and $args[2] -eq '--json') {
    Write-Output '{"email":"me@example.com","orgName":"Acme","subscriptionType":"pro"}'
    exit 0
}

if (-not $env:CLAUDE_CONFIG_DIR) {
    Write-Error 'CLAUDE_CONFIG_DIR required'
    exit 1
}

$mode = if ($env:FAKE_CLAUDE) { $env:FAKE_CLAUDE } else { 'print' }
$port = if ($env:FAKE_CLAUDE_PORT) { $env:FAKE_CLAUDE_PORT } else { '34567' }
$url = "https://claude.example/auth?redirect_uri=http%3A%2F%2Flocalhost%3A$port%2Fcallback&code=abc&state=s1"

switch ($mode) {
    'browser' {
        Start-Process -FilePath $env:BROWSER -ArgumentList ('"{0}"' -f $url) -NoNewWindow -Wait
    }
    'print' {
        Write-Output $url
    }
    'fail' {
        Write-Output 'claude exploded'
        exit 1
    }
    default {
        Write-Error "unknown FAKE_CLAUDE=$mode"
        exit 2
    }
}

$line = [Console]::In.ReadLine()
if ($null -eq $line) {
    exit 0
}
if (-not $line.Contains('#')) {
    Write-Error "expected code#state, got: $line"
    exit 3
}

Set-Content -LiteralPath (Join-Path $env:CLAUDE_CONFIG_DIR '.credentials.json') -Value @'
{"claudeAiOauth":{"accessToken":"tok-access","refreshToken":"tok-refresh","expiresAt":2000000000000,"subscriptionType":"pro"}}
'@
Set-Content -LiteralPath (Join-Path $env:CLAUDE_CONFIG_DIR '.claude.json') -Value @'
{"oauthAccount":{"emailAddress":"me@example.com","organizationName":"Acme"}}
'@
exit 0
