<#
.SYNOPSIS
  Windows Stats ölçümlerini Windows'un kendi performans sayaçlarıyla karşılaştırır.

.DESCRIPTION
  WindowsStats.exe --dump ile ölçüm alırken aynı süre boyunca Get-Counter ile Görev Yöneticisi'nin
  kullandığı sayaçları örnekler ve ortalamaları karşılaştırır:
    CPU  : \Processor Information(_Total)\% Processor Utility (Görev Yöneticisi > CPU)
    RAM  : Toplam - \Memory\Available Bytes (Görev Yöneticisi > Bellek > Kullanımda)
    Disk : \PhysicalDisk(_Total)\Disk Read/Write Bytes/sec
    Ağ   : \Network Interface(*)\Bytes Received/Sent/sec (yalnızca bilgi; sanal bağdaştırıcılar
           Windows sayaçlarında ayrıca sayılabildiği için kesin karşılaştırma yapılmaz)
  Add-Type / P-Invoke kullanmaz. Çıkış kodu 0 = tüm kontroller geçti.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\compare-taskmgr.ps1 -Seconds 20
#>
param(
    [int]$Seconds = 15,
    [string]$Exe = '',
    [double]$CpuTolerance = 8.0,   # yüzde puanı
    [double]$RamTolerance = 2.0    # yüzde puanı
)
$ErrorActionPreference = 'Stop'
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch { }
$root = Split-Path -Parent $PSScriptRoot

# Get-Counter wants counter names in the display language of Windows (Turkish here). Translate
# the English paths through the Perflib name tables: English name -> index -> local name.
$perflib = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Perflib'
$enNames = (Get-ItemProperty "$perflib\009" -Name Counter).Counter
$locNames = (Get-ItemProperty "$perflib\CurrentLanguage" -Name Counter).Counter
$byIndex = @{}
for ($i = 0; $i -lt $locNames.Count - 1; $i += 2) { if (-not $byIndex.ContainsKey($locNames[$i])) { $byIndex[$locNames[$i]] = $locNames[$i + 1] } }
$toLocal = @{}
for ($i = 0; $i -lt $enNames.Count - 1; $i += 2) {
    $name = $enNames[$i + 1]
    if ((-not $toLocal.ContainsKey($name)) -and $byIndex.ContainsKey($enNames[$i])) { $toLocal[$name] = $byIndex[$enNames[$i]] }
}
function Local([string]$englishPath) {
    # \Object(instance)\Counter
    if ($englishPath -notmatch '^\\([^(\\]+)(\([^)]*\))?\\(.+)$') { return $englishPath }
    $obj = $Matches[1]; $inst = $Matches[2]; $ctr = $Matches[3]
    if ($toLocal.ContainsKey($obj)) { $obj = $toLocal[$obj] }
    if ($toLocal.ContainsKey($ctr)) { $ctr = $toLocal[$ctr] }
    return "\$obj$inst\$ctr"
}
if (-not $Exe) { $Exe = Join-Path $root 'target\release\WindowsStats.exe' }
if (-not (Test-Path $Exe)) { throw "WindowsStats.exe bulunamadı: $Exe" }

$tmp = Join-Path $root '.devtmp'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$out = Join-Path $tmp 'dump.jsonl'
$err = Join-Path $tmp 'dump.err'

Write-Host "Ölçüm: $Seconds sn (Windows Stats --dump ve Get-Counter aynı anda)..."
$proc = Start-Process -FilePath $Exe -ArgumentList @('--dump', "$Seconds", '1000') -RedirectStandardOutput $out -RedirectStandardError $err -NoNewWindow -PassThru

$english = @(
    '\Processor Information(_Total)\% Processor Utility',
    '\Memory\Available Bytes',
    '\PhysicalDisk(_Total)\Disk Read Bytes/sec',
    '\PhysicalDisk(_Total)\Disk Write Bytes/sec',
    '\Network Interface(*)\Bytes Received/sec',
    '\Network Interface(*)\Bytes Sent/sec'
)
$counters = $english | ForEach-Object { Local $_ }
# CookedValue paths come back in the local language: index the columns by position instead
$cpuPath = $counters[0].ToLower(); $availPath = $counters[1].ToLower(); $drPath = $counters[2].ToLower(); $dwPath = $counters[3].ToLower()
$nrCounter = ($counters[4] -split '\\')[-1].ToLower(); $ntCounter = ($counters[5] -split '\\')[-1].ToLower()
# the dump primes its counters for one interval first: start sampling one second later
Start-Sleep -Milliseconds 1000
$samples = Get-Counter -Counter $counters -SampleInterval 1 -MaxSamples ($Seconds - 1)
$proc.WaitForExit(30000) | Out-Null

$total = (Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory
$cpuW = @(); $ramW = @(); $drW = @(); $dwW = @(); $nrW = @(); $ntW = @()
foreach ($s in $samples) {
    $cs = $s.CounterSamples
    $cpuW += [math]::Min(100, ($cs | Where-Object { $_.Path.ToLower().EndsWith($cpuPath) }).CookedValue)
    $avail = ($cs | Where-Object { $_.Path.ToLower().EndsWith($availPath) }).CookedValue
    $ramW += 100.0 * ($total - $avail) / $total
    $drW += ($cs | Where-Object { $_.Path.ToLower().EndsWith($drPath) }).CookedValue
    $dwW += ($cs | Where-Object { $_.Path.ToLower().EndsWith($dwPath) }).CookedValue
    $nrW += (($cs | Where-Object { $_.Path.ToLower().EndsWith($nrCounter) }) | Measure-Object -Property CookedValue -Sum).Sum
    $ntW += (($cs | Where-Object { $_.Path.ToLower().EndsWith($ntCounter) }) | Measure-Object -Property CookedValue -Sum).Sum
}

$lines = Get-Content $out | Where-Object { $_.Trim().StartsWith('{') }
$info = $null; $summary = $null; $rows = @()
foreach ($l in $lines) {
    $o = $l | ConvertFrom-Json
    if ($o.info) { $info = $o.info } elseif ($o.summary) { $summary = $o.summary } else { $rows += $o }
}
if ($rows.Count -eq 0) { throw "Windows Stats ölçüm üretmedi (bkz. $err)" }

function Avg($a) { if ($a.Count -eq 0) { return 0 } ($a | Measure-Object -Average).Average }
$cpuS = Avg ($rows | ForEach-Object { 100.0 * $_.cpu })
$ramS = Avg ($rows | ForEach-Object { 100.0 * $_.mem_pct })
$drS = Avg ($rows | ForEach-Object { $_.disk_read }); $dwS = Avg ($rows | ForEach-Object { $_.disk_write })
$nrS = Avg ($rows | ForEach-Object { $_.net_rx }); $ntS = Avg ($rows | ForEach-Object { $_.net_tx })

$fail = 0
function Line($name, $ours, $win, $unit, $tol) {
    $d = [math]::Abs($ours - $win)
    $ok = if ($tol -gt 0) { if ($d -le $tol) { 'GEÇTİ' } else { $script:fail++; 'KALDI' } } else { 'bilgi' }
    '{0,-22} Windows Stats {1,10:N1} {4}   Windows {2,10:N1} {4}   fark {3,7:N1}   {5}' -f $name, $ours, $win, $d, $unit, $ok
}
Write-Host ''
Write-Host ("İşlemci: {0} · {1} mantıksal çekirdek · GPU: {2}" -f $info.cpu, $info.cores, $info.gpu)
Line 'CPU yükü (ortalama)' $cpuS (Avg $cpuW) '%' $CpuTolerance
Line 'RAM kullanımı' $ramS (Avg $ramW) '%' $RamTolerance
Line 'Disk okuma' ($drS / 1MB) ((Avg $drW) / 1MB) 'MB/s' 0
Line 'Disk yazma' ($dwS / 1MB) ((Avg $dwW) / 1MB) 'MB/s' 0
Line 'Ağ indirme' ($nrS / 1KB) ((Avg $nrW) / 1KB) 'KB/s' 0
Line 'Ağ yükleme' ($ntS / 1KB) ((Avg $ntW) / 1KB) 'KB/s' 0
if ($summary) {
    Write-Host ''
    Write-Host ("Toplama maliyeti: örnek başına {0:N2} ms · süreç CPU {1:N3} sn / {2} örnek · özel bellek {3:N1} MB" -f $summary.avg_sample_ms, $summary.process_cpu_s, $summary.samples, ($summary.private_bytes / 1MB))
}
Write-Host ''
if ($fail -gt 0) { Write-Host "$fail kontrol KALDI" -ForegroundColor Red; exit 1 } else { Write-Host 'Tüm kontroller GEÇTİ' -ForegroundColor Green; exit 0 }
