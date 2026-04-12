$env:MINIMAX_API_KEY = $env:MINIMAX_API_KEY
$pekobotDir = "$env:USERPROFILE/.pekobot"
$DataDir = "$env:APPDATA/pekobot"
if (Test-Path $pekobotDir) { Remove-Item -Recurse -Force $pekobotDir }
if (Test-Path $DataDir) { Remove-Item -Recurse -Force $DataDir }

pekobot auth set minimax $env:MINIMAX_API_KEY 2>&1 | Out-Null
pekobot agent create testagent --provider minimax 2>&1 | Out-Null
Write-Host "=== Before enable ==="
$configPath = "$env:USERPROFILE/.pekobot/teams/default/agents/testagent/config.toml"
Get-Content $configPath

pekobot ext enable glob --target default/testagent 2>&1 | Out-Null
Write-Host "`n=== After enable glob ==="
Get-Content $configPath

pekobot agent delete testagent 2>&1 | Out-Null
