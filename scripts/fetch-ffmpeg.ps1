# Downloads a pinned FFmpeg static build and drops ffmpeg.exe into
# src-tauri/binaries/ with the target-triple name Tauri sidecars require.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$binaries = Join-Path $root "src-tauri\binaries"
$dest = Join-Path $binaries "ffmpeg-x86_64-pc-windows-msvc.exe"

if (Test-Path $dest) {
    Write-Host "ffmpeg already present: $dest"
    exit 0
}

# Latest release build (gyan.dev prunes old versioned package URLs, so a
# hard-pinned URL rots; the downloaded version is printed at the end).
$url = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
$tmp = Join-Path $env:TEMP "spindex-ffmpeg"
New-Item -ItemType Directory -Force $tmp | Out-Null
$zip = Join-Path $tmp "ffmpeg.zip"

Write-Host "Downloading $url ..."
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing

Write-Host "Extracting ..."
Expand-Archive -Path $zip -DestinationPath $tmp -Force
$exe = Get-ChildItem -Path $tmp -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
if (-not $exe) { throw "ffmpeg.exe not found in archive" }

New-Item -ItemType Directory -Force $binaries | Out-Null
Copy-Item $exe.FullName $dest -Force
Remove-Item -Recurse -Force $tmp
& $dest -version | Select-Object -First 1
Write-Host "Done: $dest"
