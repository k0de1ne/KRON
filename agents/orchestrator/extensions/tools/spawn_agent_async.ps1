$raw  = [Console]::In.ReadToEnd()
$data = $raw | ConvertFrom-Json

$agent = $data.agent
$task  = $data.task

$agents_root  = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))
$project_root = Split-Path -Parent $agents_root
$plexus       = Join-Path $project_root "target\debug\plexus.exe"
$agentdir     = Join-Path $agents_root $agent
$inbox        = Join-Path $project_root "inbox"

if (-not (Test-Path $agentdir)) {
    Write-Output "error: agent '$agent' not found (expected: $agentdir)"
    exit 1
}

$job_id    = "job-$([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())-$agent"
$config    = Join-Path $agentdir "config.toml"
$task_file = [System.IO.Path]::GetTempFileName()
$wrapper   = [System.IO.Path]::GetTempFileName() + ".ps1"

New-Item -ItemType Directory -Force $inbox | Out-Null
$utf8NoBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText($task_file, $task, $utf8NoBom)

# Wrapper sets env vars so the child's on_done hook can write to inbox
$nl = "`n"
$wc  = '$env:PLEXUS_JOB_ID = "' + $job_id + '"' + $nl
$wc += '$env:PLEXUS_INBOX  = "' + $inbox  + '"' + $nl
$wc += 'Set-Location "' + $agentdir + '"' + $nl
$wc += 'Get-Content "' + $task_file + '" -Raw -Encoding utf8 | & "' + $plexus + '" --config config.toml --thread "async-' + $job_id + '" 2>$null' + $nl
$wc += 'Remove-Item "' + $task_file + '" -Force -EA 0' + $nl
$wc += 'Remove-Item "' + $wrapper   + '" -Force -EA 0' + $nl
[System.IO.File]::WriteAllText($wrapper, $wc, [System.Text.Encoding]::UTF8)

Start-Process powershell -ArgumentList "-NoProfile", "-NonInteractive", "-File", $wrapper -WindowStyle Hidden

Write-Output $job_id
