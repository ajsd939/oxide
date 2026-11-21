# Downloads the vendored frontend assets (Monaco editor + xterm.js) into ui/.
# Required once after cloning - these folders are gitignored to keep the repo light.
# Usage: powershell -ExecutionPolicy Bypass -File scripts/fetch-assets.ps1

$ErrorActionPreference = "Stop"
$ui = Join-Path (Split-Path -Parent $PSScriptRoot) "ui"
New-Item -ItemType Directory -Force -Path "$ui\monaco", "$ui\xterm" | Out-Null

$packages = @(
    @{ Url = "https://registry.npmjs.org/monaco-editor/-/monaco-editor-0.52.2.tgz"; Tar = "monaco.tgz"; Src = "package/min"; Dest = "$ui\monaco" },
    @{ Url = "https://registry.npmjs.org/@xterm/xterm/-/xterm-5.5.0.tgz"; Tar = "xterm.tgz"; Src = "package/lib"; Dest = "$ui\xterm" },
    @{ Url = "https://registry.npmjs.org/@xterm/addon-fit/-/addon-fit-0.10.0.tgz"; Tar = "fit.tgz"; Src = "package/lib"; Dest = "$ui\xterm" }
)

$tmp = Join-Path $env:TEMP "oxide-assets"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

foreach ($pkg in $packages) {
    $tar = Join-Path $tmp $pkg.Tar
    $out = Join-Path $tmp ($pkg.Tar -replace '\.tgz$', '')
    Write-Host "Fetching $($pkg.Url)"
    Invoke-WebRequest -Uri $pkg.Url -OutFile $tar -UseBasicParsing
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    tar -xzf $tar -C $out
    New-Item -ItemType Directory -Force -Path $pkg.Dest | Out-Null
    Copy-Item -Path (Join-Path (Join-Path $out $pkg.Src) "*") -Destination $pkg.Dest -Recurse -Force
}

Write-Host "Assets ready:" 
Get-ChildItem "$ui\monaco\vs\loader.js", "$ui\xterm\xterm.js" | Select-Object FullName
