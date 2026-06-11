$job_id = $env:PLEXUS_JOB_ID
$inbox  = $env:PLEXUS_INBOX
if (-not $job_id -or -not $inbox) { exit 0 }

$raw    = [Console]::In.ReadToEnd()
$data   = $raw | ConvertFrom-Json

New-Item -ItemType Directory -Force $inbox | Out-Null
$data.output | Out-File (Join-Path $inbox "$job_id.txt") -Encoding utf8
