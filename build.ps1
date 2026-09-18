param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Debug"
)

$ErrorActionPreference = "Stop"

$ProjectRoot = $PSScriptRoot
$UiPath = Join-Path $ProjectRoot "crates\mumble-tauri\ui"
$TauriPath = Join-Path $ProjectRoot "crates\mumble-tauri"
$TargetProfile = $Configuration.ToLowerInvariant()
$OutputDirectory = Join-Path $ProjectRoot "package\windows\$TargetProfile"

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

Write-Host "构建前端（$Configuration）..." -ForegroundColor Cyan
Invoke-Checked -Command "npm" -Arguments @("run", "build") -WorkingDirectory $UiPath

if ($Configuration -eq "Debug") {
    Write-Host "构建 Windows Debug..." -ForegroundColor Cyan
    Invoke-Checked -Command "cargo" -Arguments @("build", "-p", "mumble-tauri", "--features", "custom-protocol native-screenshare") -WorkingDirectory $ProjectRoot
} else {
    Write-Host "构建 Windows Release..." -ForegroundColor Cyan
    Invoke-Checked -Command "cargo" -Arguments @("build", "--release", "-p", "mumble-tauri", "--features", "custom-protocol native-screenshare") -WorkingDirectory $ProjectRoot
}

$Executable = Join-Path $ProjectRoot "target\$TargetProfile\mumble-tauri.exe"
if (-not (Test-Path -LiteralPath $Executable)) {
    throw "编译完成但未找到可执行文件：$Executable"
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$ZipPath = Join-Path $OutputDirectory "FancyMumble-$TargetProfile.zip"
Compress-Archive -LiteralPath $Executable -DestinationPath $ZipPath -Force
if ($Configuration -eq "Debug") {
    Copy-Item -LiteralPath $ZipPath -Destination (Join-Path $ProjectRoot "package\FancyMumble-debug.zip") -Force
}

if ($Configuration -eq "Release") {
    $CargoTauri = Get-Command cargo-tauri -ErrorAction SilentlyContinue
    if (-not $CargoTauri) {
        throw '未找到 cargo-tauri。请先安装：cargo install tauri-cli --version "^2"'
    }

    Write-Host "生成 Windows MSI/NSIS 安装包..." -ForegroundColor Cyan
    Invoke-Checked -Command "cargo" -Arguments @("tauri", "build", "--bundles", "msi,nsis", "--features", "native-screenshare") -WorkingDirectory $TauriPath

    $BundleDirectory = Join-Path $ProjectRoot "target\release\bundle"
    $Installers = @(Get-ChildItem -LiteralPath $BundleDirectory -Recurse -File -Include *.msi, *.exe -ErrorAction SilentlyContinue)
    if ($Installers.Count -eq 0) {
        throw "Tauri 构建完成但未找到 MSI/NSIS 安装包：$BundleDirectory"
    }
    foreach ($Installer in $Installers) {
        Copy-Item -LiteralPath $Installer.FullName -Destination $OutputDirectory -Force
    }
}

Write-Host "Windows $Configuration 构建完成：$OutputDirectory" -ForegroundColor Green
