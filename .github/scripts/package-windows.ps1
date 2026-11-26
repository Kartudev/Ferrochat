# Packages the Windows build like a release:
#   dist/ferrochat-v<ver>-windows-x86_64.zip (+ .zip.sha256, sha256sum format)
# The zip holds one folder with ferrochat.exe, ferrochat-player.exe, the MSVC
# runtime DLLs they import (app-local, so a fresh Windows needs no redist),
# the `installed-version` marker the in-app updater looks for, README,
# LICENSE and third-party licenses. Used by release.yml and by ci.yml (which
# then installs it with install.ps1).
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$BinDir = "target/release",
    [string]$OutDir = "dist"
)
$ErrorActionPreference = "Stop"

$name = "ferrochat-v$Version-windows-x86_64"
$root = Join-Path $OutDir $name
if (Test-Path $root) { Remove-Item -Recurse -Force $root }
New-Item -ItemType Directory -Force -Path (Join-Path $root "licenses") | Out-Null

$exes = @("ferrochat.exe", "ferrochat-player.exe")
foreach ($exe in $exes) {
    $src = Join-Path $BinDir $exe
    if (-not (Test-Path $src)) { throw "missing $src" }
    Copy-Item $src $root
}

# MSVC runtime DLLs the programs import (Rust links the CRT dynamically;
# the UCRT itself ships with Windows 10+).
$dumpbin = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" `
    -latest -find "VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" | Select-Object -First 1
if (-not $dumpbin) { throw "dumpbin.exe not found" }
$dlls = @{}
foreach ($exe in $exes) {
    $deps = & $dumpbin /nologo /dependents (Join-Path $root $exe)
    Write-Host "== $exe imports:"; $deps | Where-Object { $_ -match '\.dll' } | ForEach-Object { Write-Host "  $($_.Trim())" }
    foreach ($line in $deps) {
        $d = $line.Trim()
        if ($d -match '^(?i)(vcruntime|msvcp|concrt)[0-9_]*\.dll$') { $dlls[$d.ToLower()] = $true }
        if ($d -match '^(?i)(openh264|libssl|libcrypto|opus|sqlite|sqlcipher|libstdc|libgcc|gst).*\.dll$') {
            throw "$exe links $d dynamically; it must be static"
        }
    }
}
foreach ($d in $dlls.Keys) {
    Copy-Item (Join-Path $env:SystemRoot "System32\$d") $root
    Write-Host "bundled $d"
}

Copy-Item README.md, LICENSE $root
Copy-Item assets/fonts/Inter-LICENSE.txt (Join-Path $root "licenses/Inter-OFL.txt")
Copy-Item assets/icons/LICENSE (Join-Path $root "licenses/Lucide-ISC.txt")
$registry = Join-Path $env:USERPROFILE ".cargo/registry/src"
$h264 = Get-ChildItem -Path $registry -Recurse -Filter LICENSE -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match 'openh264-sys2-[^\\/]+[\\/]upstream[\\/]LICENSE$' } | Select-Object -First 1
if (-not $h264) { throw "OpenH264 license not found" }
Copy-Item $h264.FullName (Join-Path $root "licenses/OpenH264-BSD-2-Clause.txt")
$ssl = Get-ChildItem -Path $registry -Recurse -Filter LICENSE.txt -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match 'openssl-src-[^\\/]+[\\/]openssl[\\/]LICENSE\.txt$' } | Select-Object -First 1
if (-not $ssl) { throw "OpenSSL license not found" }
Copy-Item $ssl.FullName (Join-Path $root "licenses/OpenSSL-Apache-2.0.txt")

# The in-app updater only replaces folders that carry this marker.
[IO.File]::WriteAllText((Join-Path $root "installed-version"), "$Version`n")

$zip = Join-Path $OutDir "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $root -DestinationPath $zip -CompressionLevel Optimal
$hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
[IO.File]::WriteAllText("$zip.sha256", "$hash  $name.zip`n")
Get-ChildItem $OutDir | Format-Table Name, Length
