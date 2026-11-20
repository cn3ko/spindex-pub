# Downloads the ML analyzer models into src-tauri/binaries/models (git-ignored).
#
# Default (-FromRelease, implied): pulls the three READY-TO-USE files from
# SpinDex's own `models-v1` GitHub release. This is the reproducible path —
# `spindex-key.onnx` is an ONNX conversion of the upstream checkpoint that
# requires Python 3.12 + torch to produce, so without a published copy nobody
# but the original author could build a working SpinDex.
#
# -FromUpstream: fetches the original artefacts from their own publishers
# instead (beat_this.onnx and openkeyscan3.pt), for verifying provenance or
# re-doing the conversion. You must then run scripts/convert-key-model.py.
#
# Expected sha256:
#   beat_this.onnx        C5C1466E08ABDB03FDEB50668A06F244B787D564C212490482231A9CFBE9CCBD
#   openkeyscan3.pt       5610691BE2B3AA21764C5978CA0C2524FCDBF600F09DCBDA6C2DA937745DB600
#   spindex-key.onnx      655E9F32AFE16C0C633B032F592BD2C944D25F234995E5347BA9CA78EB633363
#   spindex-key.onnx.data 29D6552FA6A142074D82E7C22CBB417F83AA8FC3C0E3F76C08515822A2038B23
#
# All model weights are MIT-licensed by their authors (see THIRD-PARTY-LICENSES.txt).

param([switch]$FromUpstream)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$dir = Join-Path $PSScriptRoot "..\src-tauri\binaries\models"
New-Item -ItemType Directory -Force $dir | Out-Null

$release = "https://github.com/cn3ko/spindex/releases/download/models-v1"

if ($FromUpstream) {
    $items = @(
        @{ Name = "beat_this.onnx"
           Sha  = "C5C1466E08ABDB03FDEB50668A06F244B787D564C212490482231A9CFBE9CCBD"
           Url  = "https://github.com/mosynthkey/beat_this_cpp/raw/main/onnx/beat_this.onnx" },
        @{ Name = "openkeyscan3.pt"
           Sha  = "5610691BE2B3AA21764C5978CA0C2524FCDBF600F09DCBDA6C2DA937745DB600"
           Url  = "https://raw.githubusercontent.com/rekordcloud/openkeyscan-analyzer/master/checkpoints/openkeyscan3.pt" }
    )
} else {
    $items = @(
        @{ Name = "beat_this.onnx"
           Sha  = "C5C1466E08ABDB03FDEB50668A06F244B787D564C212490482231A9CFBE9CCBD"
           Url  = "$release/beat_this.onnx" },
        @{ Name = "spindex-key.onnx"
           Sha  = "655E9F32AFE16C0C633B032F592BD2C944D25F234995E5347BA9CA78EB633363"
           Url  = "$release/spindex-key.onnx" },
        @{ Name = "spindex-key.onnx.data"
           Sha  = "29D6552FA6A142074D82E7C22CBB417F83AA8FC3C0E3F76C08515822A2038B23"
           Url  = "$release/spindex-key.onnx.data" }
    )
}

foreach ($item in $items) {
    $dest = Join-Path $dir $item.Name
    if (Test-Path $dest) {
        Write-Host "$($item.Name) already present, skipping"
        continue
    }
    Write-Host "Downloading $($item.Name)..."
    # Download to a temp name and rename only after the hash checks out, so an
    # interrupted or corrupted download can never be left under the real name
    # where the analyzer would load it and fail cryptically.
    $part = "$dest.part"
    Invoke-WebRequest -Uri $item.Url -OutFile $part -UseBasicParsing
    $hash = (Get-FileHash $part -Algorithm SHA256).Hash
    if ($hash -ne $item.Sha) {
        Remove-Item $part -Force
        throw "$($item.Name): sha256 mismatch`n  expected $($item.Sha)`n  got      $hash"
    }
    Move-Item $part $dest -Force
    Write-Host "  sha256 ok"
}

if ($FromUpstream) {
    Write-Host "`nDone. Now run scripts/convert-key-model.py to produce spindex-key.onnx"
    Write-Host "(needs Python 3.12 + torch; set PYTHONUTF8=1 on Windows)."
} else {
    Write-Host "`nDone - models ready in src-tauri/binaries/models."
}
