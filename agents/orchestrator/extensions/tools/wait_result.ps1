$raw  = [Console]::In.ReadToEnd()
$data = $raw | ConvertFrom-Json

$job_id  = $data.job_id
$timeout = if ($data.timeout_secs) { [int]$data.timeout_secs } else { 120 }

$agents_root  = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))
$project_root = Split-Path -Parent $agents_root
$inbox        = Join-Path $project_root "inbox"
$result_file  = Join-Path $inbox "$job_id.txt"

$deadline = [DateTime]::Now.AddSeconds($timeout)

while ([DateTime]::Now -lt $deadline) {
    if (Test-Path $result_file) {
        Get-Content $result_file -Raw -Encoding utf8
        exit 0
    }
    Start-Sleep -Milliseconds 500
}

Write-Output "timeout: result for '$job_id' not ready after ${timeout}s"
exit 1
