# Ferrochat installer / updater for Windows x86_64 (no admin rights needed).
#
#   irm https://raw.githubusercontent.com/Kartudev/Ferrochat-kartu/main/install.ps1 | iex
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/Kartudev/Ferrochat-kartu/main/install.ps1))) -Uninstall
#
# The same command installs and updates: it downloads the latest GitHub
# Release zip, verifies its SHA-256 checksum and unpacks it into
#   %LOCALAPPDATA%\Programs\Ferrochat   (ferrochat.exe, ferrochat-player.exe, ...)
# plus a Start Menu shortcut. The folder carries `installed-version`, so the
# app then updates itself. -Uninstall removes exactly that folder, the
# shortcut and the "start at login" entry (never your chats or settings).
#
# Testing only: FERRO_RELEASE_API overrides the "latest release" API URL
# (http(s):// or file:///C:/path/latest.json; file asset URLs work too).
param([switch]$Uninstall)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Repo = "Kartudev/Ferrochat-kartu"
$Crate = "ferrochat"
$AppName = "Ferrochat"
$Api = if ($env:FERRO_RELEASE_API) { $env:FERRO_RELEASE_API } else { "https://api.github.com/repos/$Repo/releases/latest" }
$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\$AppName"
$Shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "$AppName.lnk"
$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"

function Get-Url([string]$Url, [string]$OutFile) {
    if ($Url -like "file://*") {
        $path = ([Uri]$Url).LocalPath
        if ($OutFile) { Copy-Item -LiteralPath $path -Destination $OutFile } else { Get-Content -Raw -LiteralPath $path }
        return
    }
    $headers = @{ "User-Agent" = "$Crate-installer"; "Accept" = "application/vnd.github+json" }
    if ($OutFile) {
        Invoke-WebRequest -UseBasicParsing -Headers $headers -Uri $Url -OutFile $OutFile
    } else {
        (Invoke-WebRequest -UseBasicParsing -Headers $headers -Uri $Url).Content
    }
}

function Remove-Install {
    if (Get-Process -Name $Crate -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$InstallDir\*" }) {
        throw "$AppName is running. Quit it first."
    }
    if (Test-Path -LiteralPath $InstallDir) { Remove-Item -Recurse -Force -LiteralPath $InstallDir }
    if (Test-Path -LiteralPath $Shortcut) { Remove-Item -Force -LiteralPath $Shortcut }
    Remove-ItemProperty -Path $RunKey -Name $AppName -ErrorAction SilentlyContinue
    Write-Host "$AppName removed (program folder, Start Menu shortcut, start at login)."
    Write-Host "Your account data and credentials were left untouched."
}

function Install-Latest {
    $release = (Get-Url $Api) | ConvertFrom-Json
    if ($release.draft -or $release.prerelease) { throw "the latest release is not a stable release" }
    $tag = [string]$release.tag_name
    if ($tag -notmatch '^v(\d+\.\d+\.\d+)$') { throw "unexpected release tag '$tag'" }
    $version = $Matches[1]
    $name = "$Crate-v$version-windows-x86_64"
    $zipAsset = $release.assets | Where-Object { $_.name -eq "$name.zip" } | Select-Object -First 1
    $sumAsset = $release.assets | Where-Object { $_.name -eq "$name.zip.sha256" } | Select-Object -First 1
    if (-not $zipAsset -or -not $sumAsset) { throw "release $tag has no Windows build" }

    $current = Join-Path $InstallDir "installed-version"
    if ((Test-Path -LiteralPath $current) -and ((Get-Content -Raw -LiteralPath $current).Trim() -eq $version)) {
        Write-Host "$AppName $version is already installed."
        return
    }

    $work = Join-Path ([IO.Path]::GetTempPath()) "$Crate-install-$PID"
    if (Test-Path -LiteralPath $work) { Remove-Item -Recurse -Force -LiteralPath $work }
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        $zip = Join-Path $work "$name.zip"
        Write-Host "Downloading $AppName $version..."
        Get-Url $zipAsset.browser_download_url $zip
        $sumText = (Get-Url $sumAsset.browser_download_url).Trim()
        $expected = ($sumText -split '\s+')[0].ToLower()
        if ($expected -notmatch '^[0-9a-f]{64}$') { throw "the checksum file is not valid" }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLower()
        if ($actual -ne $expected) { throw "the download is damaged (checksum mismatch); nothing was installed" }

        $x = Join-Path $work "x"
        Expand-Archive -LiteralPath $zip -DestinationPath $x
        $src = Join-Path $x $name
        if (-not (Test-Path -LiteralPath (Join-Path $src "$Crate.exe"))) { throw "the release zip has an unexpected layout" }

        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        # A running .exe cannot be overwritten, but it can be renamed: move
        # the old programs aside first (the app deletes them on its next start).
        Get-ChildItem -LiteralPath $src -Filter *.exe | ForEach-Object {
            $target = Join-Path $InstallDir $_.Name
            if (Test-Path -LiteralPath $target) {
                $old = Join-Path $InstallDir ".$($_.Name).old.$PID"
                if (Test-Path -LiteralPath $old) { Remove-Item -Force -LiteralPath $old }
                Move-Item -LiteralPath $target -Destination $old
                Remove-Item -Force -LiteralPath $old -ErrorAction SilentlyContinue
            }
        }
        Copy-Item -Recurse -Force -Path (Join-Path $src "*") -Destination $InstallDir

        $shell = New-Object -ComObject WScript.Shell
        $lnk = $shell.CreateShortcut($Shortcut)
        $lnk.TargetPath = Join-Path $InstallDir "$Crate.exe"
        $lnk.WorkingDirectory = $InstallDir
        $lnk.Description = "Unofficial WhatsApp desktop client"
        $lnk.Save()
    } finally {
        Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
    }
    Write-Host "$AppName $version installed in $InstallDir (Start Menu: $AppName)."
}

if ($Uninstall) { Remove-Install } else { Install-Latest }
