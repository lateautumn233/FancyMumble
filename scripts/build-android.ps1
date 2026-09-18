param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Debug",
    [switch]$Bundle
)

$ErrorActionPreference = "Stop"

$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot ".." )).Path
$TauriPath = Join-Path $ProjectRoot "crates\mumble-tauri"
$TargetProfile = $Configuration.ToLowerInvariant()
$OutputDirectory = Join-Path $ProjectRoot "package\android\$TargetProfile"

function Invoke-Checked {
    param(
        [string]$Command,
        [string[]]$Arguments,
        [string]$WorkingDirectory
    )

    Push-Location $WorkingDirectory
    try {
        & $Command @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "命令执行失败（退出码 $LASTEXITCODE）：$Command $($Arguments -join ' ')"
        }
    }
    finally {
        Pop-Location
    }
}

$CargoTauri = Get-Command cargo-tauri -ErrorAction SilentlyContinue
if (-not $CargoTauri) {
    throw '未找到 cargo-tauri。请先安装：cargo install tauri-cli --version "^2"'
}

$Arguments = @("tauri", "android", "build")
if ($Configuration -eq "Debug") {
    $Arguments += "--debug"
}
if ($Bundle) {
    $Arguments += "--aab"
} else {
    $Arguments += "--apk"
}

Write-Host "构建 Android $Configuration（$([string]::Join(' ', $Arguments))）..." -ForegroundColor Cyan
Invoke-Checked -Command "cargo" -Arguments $Arguments -WorkingDirectory $TauriPath

$AndroidOutputs = Join-Path $TauriPath "gen\android\app\build\outputs"
$Patterns = if ($Bundle) { @("*.aab") } else { @("*.apk") }
$Artifacts = @(Get-ChildItem -LiteralPath $AndroidOutputs -Recurse -File -Include $Patterns -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -notmatch "\\build\\intermediates\\" })
if ($Artifacts.Count -eq 0) {
    throw "Android 构建完成但未找到输出文件：$AndroidOutputs"
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
foreach ($Artifact in $Artifacts) {
    Copy-Item -LiteralPath $Artifact.FullName -Destination $OutputDirectory -Force
}

Write-Host "Android $Configuration 构建完成：$OutputDirectory" -ForegroundColor Green
