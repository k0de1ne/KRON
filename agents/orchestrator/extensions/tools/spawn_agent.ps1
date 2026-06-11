$raw  = [Console]::In.ReadToEnd()
$data = $raw | ConvertFrom-Json

$agent = $data.agent
$task  = $data.task

# PSScriptRoot: agents/orchestrator/extensions/tools
# 3 up → agents/
# 4 up → project root (for plexus binary)
$agents_root  = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))
$project_root = Split-Path -Parent $agents_root
$plexus       = Join-Path $project_root "target\debug\plexus.exe"
$agentdir     = Join-Path $agents_root $agent

if (-not (Test-Path $agentdir)) {
    Write-Output "error: agent '$agent' not found (expected: $agentdir)"
    exit 1
}

$key    = "thread-$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())"
$config = Join-Path $agentdir "config.toml"

Push-Location $agentdir
try {
    & $plexus --config $config $task --thread $key
} finally {
    Pop-Location
}
