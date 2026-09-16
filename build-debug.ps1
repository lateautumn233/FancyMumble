$ErrorActionPreference = "Stop"

$ProjectRoot = $PSScriptRoot
$UiPath = Join-Path $ProjectRoot "crates\mumble-tauri\ui"

Write-Host "Building frontend..." -ForegroundColor Cyan
Push-Location $UiPath
try {
    npm run build
}
finally {
    Pop-Location
}

Write-Host "Building FancyMumble Debug (native screen share enabled)..." -ForegroundColor Cyan
Push-Location $ProjectRoot
try {
    cargo build -p mumble-tauri --features "custom-protocol native-screenshare"
}
finally {
    Pop-Location
}

$Output = Join-Path $ProjectRoot "target\debug\mumble-tauri.exe"
if (-not (Test-Path -LiteralPath $Output)) {
    throw "Build completed but executable was not found: $Output"
}

$PackageDirectory = Join-Path $ProjectRoot "package"
$PackagePath = Join-Path $PackageDirectory "FancyMumble-debug.zip"

New-Item -ItemType Directory -Path $PackageDirectory -Force | Out-Null
Compress-Archive -LiteralPath $Output -DestinationPath $PackagePath -Force

if (-not (Test-Path -LiteralPath $PackagePath)) {
    throw "Packaging completed but ZIP file was not found: $PackagePath"
}

Write-Host "Debug build complete: $Output" -ForegroundColor Green
Write-Host "Debug package complete: $PackagePath" -ForegroundColor Green
