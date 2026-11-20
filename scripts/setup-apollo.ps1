# One-time setup for the SP3 ML restoration sidecar (Apollo, CC BY-SA 4.0).
# Populates src-tauri/binaries/apollo/ (git-ignored):
#   Apollo/            model code (JusperLee/Apollo)
#   pytorch_model.bin  weights (huggingface.co/JusperLee/Apollo)
#   venv/              Python 3.12 venv with CPU torch
#   run_restore.py     the committed runner, copied from scripts/apollo/
# The Restore feature stays hidden in SpinDex until all four exist.
# Requires: python 3.12 on PATH, git, internet. Idempotent — safe to re-run.
param([string]$Dest)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

# In a dev checkout install into src-tauri/binaries (searched first); for an
# installed SpinDex there is no such directory, so fall back to the same
# user-writable component directory install-components.ps1 uses.
if ($Dest) {
    $apollo = $Dest
} elseif (Test-Path (Join-Path $root "src-tauri\binaries")) {
    $apollo = Join-Path $root "src-tauri\binaries\apollo"
} else {
    $apollo = Join-Path $env:APPDATA "app.spindex.desktop\bin\apollo"
}
Write-Host "Installing Apollo into $apollo"
New-Item -ItemType Directory -Force $apollo | Out-Null

if (-not (Test-Path (Join-Path $apollo "Apollo\look2hear"))) {
    Write-Host "Cloning Apollo..."
    git clone --depth 1 https://github.com/JusperLee/Apollo.git (Join-Path $apollo "Apollo")
}

$venvPy = Join-Path $apollo "venv\Scripts\python.exe"
if (-not (Test-Path $venvPy)) {
    Write-Host "Creating venv..."
    python -m venv (Join-Path $apollo "venv")
}
Write-Host "Installing packages (CPU torch, ~200 MB)..."
& $venvPy -m pip install --quiet --upgrade pip
& $venvPy -m pip install --quiet torch torchaudio --index-url https://download.pytorch.org/whl/cpu
& $venvPy -m pip install --quiet numpy omegaconf huggingface_hub pyyaml tqdm

$ckpt = Join-Path $apollo "pytorch_model.bin"
if (-not (Test-Path $ckpt) -or (Get-Item $ckpt).Length -eq 0) {
    Write-Host "Downloading weights (~66 MB)..."
    & $venvPy -c "from huggingface_hub import hf_hub_download; import shutil; p = hf_hub_download('JusperLee/Apollo', 'pytorch_model.bin'); shutil.copy(p, r'$ckpt')"
}

Copy-Item (Join-Path $PSScriptRoot "apollo\run_restore.py") (Join-Path $apollo "run_restore.py") -Force

# Smoke test: model imports and loads.
Write-Host "Verifying (imports + model load, may take a minute)..."
$env:PYTHONPATH = Join-Path $apollo "Apollo"
& $venvPy -c "import sys, os; sys.path.insert(0, os.path.join(r'$apollo', 'Apollo')); import look2hear.models; m = look2hear.models.BaseModel.from_pretrain(r'$ckpt', sr=44100, win=20, feature_dim=256, layer=6); print('OK: Apollo ready')"
