param(
    [int]$DurationSeconds = 180,
    [int]$SampleIntervalSeconds = 2,
    [string]$Output = ".\\seelen-memory.csv"
)

$ErrorActionPreference = "Stop"
if ($DurationSeconds -lt 1 -or $SampleIntervalSeconds -lt 1) {
    throw "DurationSeconds and SampleIntervalSeconds must be positive."
}

$rows = [System.Collections.Generic.List[object]]::new()
$stopAt = (Get-Date).AddSeconds($DurationSeconds)

Write-Host "Sampling Seelen UI and WebView2 process memory..."
Write-Host "Duration: $DurationSeconds sec | Interval: $SampleIntervalSeconds sec"
Write-Host "Output: $Output"
Write-Host "Close unrelated Edge/WebView2 applications before measuring for cleaner results."

while ((Get-Date) -lt $stopAt) {
    $timestamp = (Get-Date).ToString("o")
    $processes = Get-Process -ErrorAction SilentlyContinue |
        Where-Object {
            $_.ProcessName -match '^(seelen-ui|slu|slu-service|msedgewebview2)$'
        }

    foreach ($process in $processes) {
        $rows.Add([pscustomobject]@{
            Timestamp = $timestamp
            ProcessId = $process.Id
            ProcessName = $process.ProcessName
            WorkingSetMB = [math]::Round($process.WorkingSet64 / 1MB, 2)
            PrivateMemoryMB = [math]::Round($process.PrivateMemorySize64 / 1MB, 2)
            PagedMemoryMB = [math]::Round($process.PagedMemorySize64 / 1MB, 2)
            CPUSeconds = [math]::Round($process.CPU, 2)
        })
    }

    Start-Sleep -Seconds $SampleIntervalSeconds
}

$rows | Export-Csv -Path $Output -NoTypeInformation -Encoding UTF8
$summary = $rows |
    Group-Object ProcessName |
    ForEach-Object {
        [pscustomobject]@{
            Process = $_.Name
            Samples = $_.Count
            AvgWorkingSetMB = [math]::Round(($_.Group | Measure-Object WorkingSetMB -Average).Average, 2)
            PeakWorkingSetMB = [math]::Round(($_.Group | Measure-Object WorkingSetMB -Maximum).Maximum, 2)
            AvgPrivateMemoryMB = [math]::Round(($_.Group | Measure-Object PrivateMemoryMB -Average).Average, 2)
            PeakPrivateMemoryMB = [math]::Round(($_.Group | Measure-Object PrivateMemoryMB -Maximum).Maximum, 2)
        }
    }

Write-Host "`nPer-process summary:"
$summary | Format-Table -AutoSize
Write-Host "`nCSV saved to $Output"
