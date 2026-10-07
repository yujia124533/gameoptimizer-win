# Build GameOptimizer with w64devkit (portable GCC), one command.
# Usage:  powershell -ExecutionPolicy Bypass -File tools\build_w64devkit.ps1
# Output: build\gopt_cli.exe, build\gopt_gui.exe
# NOTE: keep this file ASCII-only. Windows PowerShell 5.1 reads BOM-less scripts as ANSI,
#       so non-ASCII text here can break parsing or the toolchain lookup.
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$toolchainRoot = Join-Path (Split-Path -Parent $root) "toolchains"

# Locate w64devkit (nested bin layout, e.g. toolchains\w64devkit\w64devkit)
$gpp = Get-ChildItem $toolchainRoot -Recurse -Depth 3 -Filter g++.exe -ErrorAction SilentlyContinue |
       Where-Object { $_.FullName -like '*\w64devkit*' } |
       Select-Object -First 1 -ExpandProperty FullName
if (-not $gpp) {
    $cand = "C:\w64devkit\bin\g++.exe"
    if (Test-Path $cand) { $gpp = $cand }
}
if (-not $gpp) {
    Write-Host "w64devkit g++.exe not found. Download: https://github.com/skeeto/w64devkit/releases" -ForegroundColor Red
    exit 1
}
Write-Host "toolchain: $gpp"
# GCC needs its bin on PATH to find as/ld/cc1
$wbin = Split-Path -Parent $gpp
$env:PATH = "$wbin;$env:PATH"

$build = Join-Path $root "build"
New-Item -ItemType Directory -Force -Path $build | Out-Null

# Windows version resources (windres), linked into each exe
$windres = Join-Path $wbin "windres.exe"
& $windres resources\resource.rc -O coff -o $build\resource.o
& $windres resources\gui_resource.rc -O coff -o $build\gui_resource.o

$SRCS = @(
    'src\hardware\HardwareProfile.cpp'
    'src\hardware\HardwareDetector.cpp'
    'src\hal\HAL.cpp'
    'src\preset\GameOptimizationPreset.cpp'
    'src\rollback\SecurityRollback.cpp'
    'src\config\GameConfig.cpp'
    'src\tuning\SystemTuner.cpp'
    'src\tuning\StartupManager.cpp'
    'src\license\License.cpp'
    'src\core\AppCore.cpp'
)

# v1.1.0 UI-only sources: GUI target only (CLI/self-test do not link them)
$UISRCS = @(
    'src\gui\ui_theme.cpp'
    'src\gui\ui_widgets.cpp'
    'src\gui\page_dashboard.cpp'
    'src\gui\page_game.cpp'
    'src\gui\page_tune.cpp'
    'src\gui\page_process.cpp'
    'src\gui\page_startup.cpp'
)

& $gpp -std=c++17 -O2 -Wall -Wextra -Isrc $SRCS `
    tools\cli_main.cpp `
    build\resource.o `
    -o build\gopt_cli.exe -ldxgi -ladvapi32 -lpowrprof

if ($LASTEXITCODE -ne 0) { Write-Host "CLI build failed (exit $LASTEXITCODE)" -ForegroundColor Red; exit $LASTEXITCODE }
Write-Host "built: $build\gopt_cli.exe" -ForegroundColor Green

# Native GUI (multi-page shell + page modules)
& $gpp -std=c++17 -O2 -Wall -Wextra -Isrc $SRCS $UISRCS `
    src\gui\gopt_gui.cpp `
    build\gui_resource.o `
    -o build\gopt_gui.exe -mwindows -luser32 -lgdi32 -lcomdlg32 -lcomctl32 -lshell32 -lpsapi `
    -ldxgi -ladvapi32 -lpowrprof

if ($LASTEXITCODE -ne 0) { Write-Host "GUI build failed (exit $LASTEXITCODE)" -ForegroundColor Red; exit $LASTEXITCODE }
Write-Host "built: $build\gopt_gui.exe" -ForegroundColor Green

# Self-test binary (real-API round trip), used by the release checklist and verification
& $gpp -std=c++17 -O2 -Wall -Wextra -Isrc $SRCS `
    tools\verify_real.cpp `
    -o build\gopt_verify.exe -ldxgi -ladvapi32 -lpowrprof -lpsapi

if ($LASTEXITCODE -ne 0) { Write-Host "verify build failed (exit $LASTEXITCODE)" -ForegroundColor Red; exit $LASTEXITCODE }
Write-Host "built: $build\gopt_verify.exe" -ForegroundColor Green
