# Generates SHA256SUMS.txt for the current version's Windows installers.
# Run after `npm run tauri build`, then attach the file to the GitHub release:
#   gh release upload vX.Y.Z SHA256SUMS.txt --clobber
#
# Usage (from the project root):
#   powershell -ExecutionPolicy Bypass -File scripts\checksums.ps1

$ErrorActionPreference = "Stop"

# Read the current version so we only hash this build's installers, not stale
# artifacts from previous builds that linger in the bundle folder.
$confPath = Join-Path $PSScriptRoot "..\src-tauri\tauri.conf.json"
$version = (Get-Content $confPath -Raw | ConvertFrom-Json).version
if (-not $version) { Write-Error "Couldn't read version from tauri.conf.json"; exit 1 }

$bundle = Join-Path $PSScriptRoot "..\src-tauri\target\release\bundle"
$installers = Get-ChildItem -Path $bundle -Recurse -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -like "*_$version*-setup.exe" -or $_.Name -like "*_${version}_*.msi" }

if (-not $installers) {
    Write-Error "No v$version installers found under $bundle. Run 'npm run tauri build' first."
    exit 1
}

$out = Join-Path (Get-Location) "SHA256SUMS.txt"
$lines = foreach ($f in $installers) {
    $hash = (Get-FileHash $f.FullName -Algorithm SHA256).Hash.ToLower()
    "$hash *$($f.Name)"
}

$lines | Set-Content -Path $out -Encoding ascii
Write-Host "Wrote $out (v$version)"
Get-Content $out
