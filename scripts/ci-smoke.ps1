param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$WorkDirectory
)
$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path $Executable).Path
New-Item -ItemType Directory -Force $WorkDirectory | Out-Null
$work = (Resolve-Path $WorkDirectory).Path
$previousHome = $env:WINDOWS_STATS_HOME
$env:WINDOWS_STATS_HOME = Join-Path $work 'home'
New-Item -ItemType Directory -Force $env:WINDOWS_STATS_HOME | Out-Null

function Invoke-CheckedCli([string[]]$CliArguments, [string]$Name) {
    $stdout = Join-Path $work "$Name.stdout.txt"
    $stderr = Join-Path $work "$Name.stderr.txt"
    $process = Start-Process -FilePath $exe -ArgumentList $CliArguments -PassThru -NoNewWindow -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    try {
        if (-not $process.WaitForExit(30000)) {
            $process.Kill()
            throw "$Name exceeded its 30-second timeout"
        }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) { throw "$Name exited with code $($process.ExitCode)" }
        return Get-Content $stdout
    } finally {
        $process.Dispose()
    }
}

try {
    $expected = (Select-String Cargo.toml '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
    $version = (Invoke-CheckedCli @('--version') 'version') -join "`n"
    if ($version.Trim() -ne "Windows Stats $expected") { throw "Unexpected CLI version: $version" }
    $lines = @(Invoke-CheckedCli @('--dump', '2', '1000') 'samples' | Where-Object { $_.Trim() })
    if ($lines.Count -ne 2) { throw "Expected 2 JSON samples, received $($lines.Count)" }
    foreach ($line in $lines) {
        $sample = $line | ConvertFrom-Json
        if ($null -eq $sample.cpu -or $sample.cpu -lt 0 -or $sample.cpu -gt 1) { throw 'CPU sample is invalid' }
        if (@($sample.cpu_cores).Count -lt 1) { throw 'CPU cores were not sampled' }
        if ($sample.mem_total -le 0 -or $sample.mem_used -le 0 -or $sample.mem_used -gt $sample.mem_total) { throw 'Memory sample is invalid' }
        if ($null -eq $sample.mem_pct -or $sample.mem_pct -lt 0 -or $sample.mem_pct -gt 1) { throw 'Memory fraction is invalid' }
    }
    Write-Host 'CLI smoke passed: version and two real CPU/memory samples. No UI or installer was started.'
    if ($env:GITHUB_STEP_SUMMARY) {
        'CLI smoke: version and two real CPU/memory samples passed. UI, installer and physical hardware sensors were not tested.' |
            Out-File $env:GITHUB_STEP_SUMMARY -Append
    }
} finally {
    $env:WINDOWS_STATS_HOME = $previousHome
}
