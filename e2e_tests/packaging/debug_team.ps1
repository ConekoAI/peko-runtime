$testDir = Join-Path $env:TEMP ('pekobot_test_' + [Guid]::NewGuid().ToString().Substring(0,8))
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
peko team create testteam2 --description 'Test team for packaging' 2>&1 | Out-Null
peko agent create 'testteam2/agent1' --provider minimax 2>&1 | Out-Null
$exportPath = Join-Path $testDir 'team_export.team'
peko team export testteam2 -o $exportPath --json 2>&1 | Out-Null
Write-Host "=== Contents of .team package ==="
tar -tzf $exportPath
Write-Host ""
Write-Host "=== team.toml content ==="
tar -xzf $exportPath -O team/team.toml 2>$null
if ($LASTEXITCODE -ne 0) {
    Write-Host "team/team.toml not found in package"
}
peko team remove testteam2 --force 2>&1 | Out-Null
