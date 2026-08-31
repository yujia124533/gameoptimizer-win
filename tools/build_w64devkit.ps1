# 使用 w64devkit（便携 GCC）一键构建 GameOptimizer
# 用法:  powershell -ExecutionPolicy Bypass -File tools\build_w64devkit.ps1
# 输出:  build\gopt_cli.exe
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$toolchainRoot = Join-Path (Split-Path -Parent $root) "toolchains"

# 在常见位置查找 w64devkit（含嵌套 bin 目录，如 toolchains\w64devkit\w64devkit）
$gpp = Get-ChildItem $toolchainRoot -Recurse -Depth 3 -Filter g++.exe -ErrorAction SilentlyContinue |
       Where-Object { $_.FullName -like '*\w64devkit*' } |
       Select-Object -First 1 -ExpandProperty FullName
if (-not $gpp) {
    $cand = "C:\w64devkit\bin\g++.exe"
    if (Test-Path $cand) { $gpp = $cand }
}
if (-not $gpp) {
    Write-Host "未找到 w64devkit 的 g++.exe，请先下载：https://github.com/skeeto/w64devkit/releases" -ForegroundColor Red
    exit 1
}
Write-Host "使用工具链: $gpp"
# GCC 需要其 bin 在 PATH 上以找到 as/ld/cc1
$wbin = Split-Path -Parent $gpp
$env:PATH = "$wbin;$env:PATH"

$build = Join-Path $root "build"
New-Item -ItemType Directory -Force -Path $build | Out-Null

# 编译 Windows 版本资源（windres），随 exe 链接
$windres = Join-Path $wbin "windres.exe"
& $windres resources\resource.rc -O coff -o $build\resource.o

& $gpp -std=c++17 -O2 -Wall -Wextra -Isrc `
    src\hardware\HardwareProfile.cpp `
    src\hardware\HardwareDetector.cpp `
    src\hal\HAL.cpp `
    src\preset\GameOptimizationPreset.cpp `
    src\rollback\SecurityRollback.cpp `
    src\config\GameConfig.cpp `
    src\tuning\SystemTuner.cpp `
    src\tuning\StartupManager.cpp `
    src\license\License.cpp `
    src\core\AppCore.cpp `
    tools\cli_main.cpp `
    build\resource.o `
    -o build\gopt_cli.exe -ldxgi -ladvapi32 -lpowrprof

if ($LASTEXITCODE -ne 0) { Write-Host "编译失败（exit $LASTEXITCODE）" -ForegroundColor Red; exit $LASTEXITCODE }
Write-Host "构建成功: $build\gopt_cli.exe" -ForegroundColor Green
