# SpinDex — install the optional components
#
# SpinDex ships only its own binaries. One component it uses is licensed under
# the GPL, and rather than redistribute it (which would carry source-offer
# obligations and put someone else's licence on this installer) SpinDex fetches
# it onto your machine from its own publisher:
#
#   ffmpeg  (GPLv3, https://www.gyan.dev/ffmpeg/builds/)
#           audio decoding for BPM/key/quality analysis, aiff playback, the
#           grid editor's waveform, and the enhance / steady-tempo / restore
#           features
#
# It lands in %APPDATA%\app.spindex.desktop\bin, which is yours, survives an
# app upgrade, and needs no administrator rights. Nothing here is required:
# without it SpinDex still scans, searches, edits tags and manages crates.
#
# Usage:  powershell -ExecutionPolicy Bypass -File install-components.ps1
#         powershell -ExecutionPolicy Bypass -File install-components.ps1 -Only ffmpeg

param(
    [ValidateSet("all", "ffmpeg")]
    [string]$Only = "all"
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$dest = Join-Path $env:APPDATA "app.spindex.desktop\bin"
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Write-Host "Installing into $dest`n"

function Get-FromZip {
    param([string]$Url, [string]$ExeName, [string]$Label)

    $target = Join-Path $dest $ExeName
    if (Test-Path $target) {
        Write-Host "$Label already installed - skipping ($target)"
        return
    }

    $tmp = Join-Path $env:TEMP ("spindex-" + [IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    try {
        $zip = Join-Path $tmp "download.zip"
        Write-Host "Downloading $Label from $Url"
        Invoke-WebRequest -Uri $Url -OutFile $zip -UseBasicParsing

        Expand-Archive -Path $zip -DestinationPath $tmp -Force
        $exe = Get-ChildItem -Path $tmp -Recurse -Filter $ExeName | Select-Object -First 1
        if (-not $exe) { throw "$ExeName not found inside the archive" }

        # Copy to a temp name first, then rename: an interrupted copy can never
        # leave a half-written exe under the real name, which discovery would
        # then find and fail on cryptically at spawn time.
        $staged = "$target.part"
        Copy-Item $exe.FullName $staged -Force
        Move-Item $staged $target -Force
        Write-Host "Installed $target"
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

if ($Only -eq "all" -or $Only -eq "ffmpeg") {
    # gyan.dev prunes old versioned package URLs, so a pinned URL rots; this
    # alias always resolves to the current release build.
    Get-FromZip -Url "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip" `
                -ExeName "ffmpeg.exe" -Label "ffmpeg (GPLv3)"
}

# The analyzer links against ONNX Runtime, which imports MSVCP140.dll from the
# Microsoft Visual C++ Redistributable. That is NOT part of Windows, and it
# cannot be statically linked in (the prebuilt ONNX Runtime library is built
# against the dynamic CRT). Most machines already have it via some other app,
# so this is a check rather than an install: it needs administrator rights,
# which this script deliberately does not require.
$vcRuntime = Join-Path $env:SystemRoot "System32\MSVCP140.dll"
if (-not (Test-Path $vcRuntime)) {
    Write-Host ""
    Write-Warning "The Microsoft Visual C++ Redistributable (x64) is missing."
    Write-Host "  BPM/key analysis cannot start without it. Everything else works."
    Write-Host "  Install it once from: https://aka.ms/vs/17/release/vc_redist.x64.exe"
} else {
    Write-Host "`nVisual C++ Redistributable: present"
}

Write-Host "`nDone. Restart SpinDex to pick up the new component."
Write-Host "This component is licensed by its own authors, not by SpinDex:"
Write-Host "  ffmpeg  GPLv3  https://ffmpeg.org/  (source: https://www.gyan.dev/ffmpeg/builds/)"
