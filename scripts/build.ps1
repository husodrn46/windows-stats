<#
.SYNOPSIS
  Windows Stats derleme betiği / build script.

.DESCRIPTION
  Proje içindeki araç zincirini (.toolchain: Rust 1.99 GNU + llvm-mingw windres) kullanır; yoksa
  PATH'teki cargo'ya döner. Sürüm (release) derler, isteğe bağlı testleri çalıştırır ve
  dist\WindowsStats-<sürüm>\ klasörünü (exe + lisanslar + README) hazırlayıp zip'ler.
  Yönetici izni gerektirmez, sistem ayarlarına dokunmaz.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Test
#>
param(
    [switch]$Test,
    [switch]$NoPackage
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# --- toolchain -------------------------------------------------------------------------------
$tc = Join-Path $root '.toolchain'
if (Test-Path (Join-Path $tc 'cargo\bin\cargo.exe')) {
    $env:RUSTUP_HOME = Join-Path $tc 'rustup'
    $env:CARGO_HOME = Join-Path $tc 'cargo'
    $env:PATH = (Join-Path $tc 'cargo\bin') + ';' + $env:PATH
    Write-Host "Araç zinciri: $tc"
} elseif (Get-Command cargo -ErrorAction SilentlyContinue) {
    Write-Host 'Araç zinciri: PATH içindeki cargo'
} else {
    Write-Error 'cargo bulunamadı. README.md > "Derleme" bölümündeki adımlarla Rust kurun.'
}
$windres = Join-Path $tc 'llvm-mingw\bin\x86_64-w64-mingw32-windres.exe'
if ((-not $env:WINDRES) -and (Test-Path $windres)) {
    $env:WINDRES = $windres
    $env:AR = Join-Path $tc 'llvm-mingw\bin\llvm-ar.exe'
}

# --- build -----------------------------------------------------------------------------------
& cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build başarısız ($LASTEXITCODE)" }
if ($Test) {
    & cargo test --release
    if ($LASTEXITCODE -ne 0) { throw "testler başarısız ($LASTEXITCODE)" }
}

$exe = Join-Path $root 'target\release\WindowsStats.exe'
if (-not (Test-Path $exe)) { throw "çıktı yok: $exe" }
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "Derlendi: $exe ($version, $([math]::Round((Get-Item $exe).Length / 1KB)) KB)"

# --- package ---------------------------------------------------------------------------------
if (-not $NoPackage) {
    $dist = Join-Path $root "dist\WindowsStats-$version"
    if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    Copy-Item $exe $dist
    Copy-Item (Join-Path $root 'LICENSE') (Join-Path $dist 'LICENSE.txt')
    Copy-Item (Join-Path $root 'THIRD-PARTY-NOTICES.md') (Join-Path $dist 'THIRD-PARTY-NOTICES.txt')
    Copy-Item (Join-Path $root 'README.md') (Join-Path $dist 'README.md')
    Copy-Item (Join-Path $root 'README.tr.md') (Join-Path $dist 'README.tr.md')
    Copy-Item -Recurse (Join-Path $root 'licenses') (Join-Path $dist 'licenses')
    Copy-Item (Join-Path $root 'scripts\Kur.cmd') (Join-Path $dist 'Kur.cmd')
    # optional sensor helper (LibreHardwareMonitorLib): needs the .NET SDK to build
    if (Get-Command dotnet -ErrorAction SilentlyContinue) {
        & dotnet build (Join-Path $root 'helpers\sensors\WindowsStats.Sensors.csproj') -c Release --nologo -v quiet
        if ($LASTEXITCODE -ne 0) { throw "sensor helper build failed ($LASTEXITCODE)" }
        $helper = Join-Path $root 'target\sensors\bin\Release\net48'
        $sdir = Join-Path $dist 'sensors'
        New-Item -ItemType Directory -Force -Path $sdir | Out-Null
        Get-ChildItem $helper -Include '*.exe', '*.dll', '*.config' -File -Recurse | Copy-Item -Destination $sdir
    } else {
        Write-Warning 'dotnet not found: package built without the sensor helper'
    }
    $zip = Join-Path $root "dist\WindowsStats-$version-x64.zip"
    if (Test-Path $zip) { Remove-Item -Force $zip }
    Compress-Archive -Path (Join-Path $dist '*') -DestinationPath $zip
    Write-Host "Paket: $zip"
}
