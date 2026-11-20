# Sets the SpinDex version in the three files that must agree, and verifies
# afterwards that they do. Getting these out of step has already shipped once:
# the landing page advertised 0.1.0 while the installer was 0.2.0, so the
# download button 404'd.
#
# Usage:  powershell -ExecutionPolicy Bypass -File scripts\bump-version.ps1 0.2.1
#         powershell -ExecutionPolicy Bypass -File scripts\bump-version.ps1 -Check

param(
    [Parameter(Position = 0)][string]$Version,
    [switch]$Check
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

$pkg   = Join-Path $root "package.json"
$conf  = Join-Path $root "src-tauri\tauri.conf.json"
$cargo = Join-Path $root "src-tauri\Cargo.toml"

function Get-Versions {
    [ordered]@{
        "package.json"     = ([regex]'"version":\s*"([^"]+)"').Match((Get-Content $pkg -Raw)).Groups[1].Value
        "tauri.conf.json"  = ([regex]'"version":\s*"([^"]+)"').Match((Get-Content $conf -Raw)).Groups[1].Value
        "Cargo.toml"       = ([regex]'(?m)^version\s*=\s*"([^"]+)"').Match((Get-Content $cargo -Raw)).Groups[1].Value
    }
}

if ($Check -or -not $Version) {
    $v = Get-Versions
    $v.GetEnumerator() | ForEach-Object { "{0,-18} {1}" -f $_.Key, $_.Value }
    # @(...) forces an array: a single string would otherwise index by character.
    $distinct = @($v.Values | Sort-Object -Unique)
    if ($distinct.Count -ne 1) { Write-Error "versions disagree"; exit 1 }
    Write-Host "`nAll three agree: $($distinct[0])"
    if (-not $Version) { exit 0 }
}

if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    throw "version must look like 1.2.3, got '$Version'"
}

# Write UTF-8 WITHOUT a BOM. PowerShell 5.1's `Set-Content -Encoding utf8`
# emits one, which corrupts these files: a BOM at the head of package.json or
# Cargo.toml shows up as a whole-file diff and can break stricter parsers.
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
function Write-Text { param($Path, $Text) [IO.File]::WriteAllText($Path, $Text, $utf8NoBom) }

# Cargo.toml: only the [package] version, which is the first `version = "..."`
# in the file. Dependency versions further down must not be touched.
$c = Get-Content $cargo -Raw
$c = [regex]::Replace($c, '(?m)^version\s*=\s*"[^"]+"', "version = `"$Version`"", 1)
Write-Text $cargo $c

foreach ($f in @($pkg, $conf)) {
    $t = Get-Content $f -Raw
    $t = [regex]::Replace($t, '"version":\s*"[^"]+"', "`"version`": `"$Version`"", 1)
    Write-Text $f $t
}

Write-Host "Set to ${Version}:`n"
(Get-Versions).GetEnumerator() | ForEach-Object { "  {0,-18} {1}" -f $_.Key, $_.Value }
Write-Host "`nNext: npm run tauri build, then see RELEASING.md"
