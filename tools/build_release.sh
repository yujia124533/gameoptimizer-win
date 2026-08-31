#!/usr/bin/env bash
# GameOptimizer 发布构建（MSYS2 MINGW64 / 兼容 MinGW 环境）
# 产物: build/gopt_cli.exe, gopt_gui.exe, gopt_verify.exe, GameOptimizer-setup.exe
#       + release/GameOptimizer-portable.zip
set -euo pipefail

SRCS="src/hardware/HardwareProfile.cpp src/hardware/HardwareDetector.cpp \
src/hal/HAL.cpp src/preset/GameOptimizationPreset.cpp \
src/rollback/SecurityRollback.cpp src/config/GameConfig.cpp \
src/tuning/SystemTuner.cpp \
src/tuning/StartupManager.cpp \
src/license/License.cpp src/core/AppCore.cpp"

mkdir -p build release/GameOptimizer

# 使用说明（docs 中的跟踪副本）与自检脚本
cp -f docs/BEFORE_USE_README.txt release/GameOptimizer/BEFORE_USE_README.txt
cat > "release/GameOptimizer/自检.cmd" <<'EOF'
@echo off
chcp 65001 >nul
echo [1/2] 机制自检（真实进程 优先级/亲和性 设置+恢复）...
gopt_verify.exe
echo.
echo [2/2] 硬件指纹与预设...
gopt_cli.exe status
echo.
pause
EOF

echo "== 编译 CLI =="
windres resources/resource.rc -O coff -o build/resource.o
g++ -std=c++17 -O2 -static -Isrc $SRCS tools/cli_main.cpp build/resource.o \
    -o build/gopt_cli.exe -ldxgi -ladvapi32 -lpowrprof

echo "== 编译 GUI =="
g++ -std=c++17 -O2 -static -Isrc src/gui/gopt_gui.cpp $SRCS build/resource.o \
    -o build/gopt_gui.exe -mwindows -luser32 -lgdi32 -lcomdlg32 \
    -ldxgi -ladvapi32 -lpowrprof

echo "== 编译 自检程序 =="
g++ -std=c++17 -O2 -static -Isrc $SRCS tools/verify_real.cpp \
    -o build/gopt_verify.exe -ldxgi -ladvapi32 -lpowrprof

echo "== 编译 安装程序 =="
windres resources/installer_resource.rc -O coff -o build/installer_resource.o
g++ -std=c++17 -O2 -static resources/installer.cpp build/installer_resource.o \
    -o build/GameOptimizer-setup.exe -lshell32 -lole32 -luser32 -ladvapi32 -luuid

echo "== 打包 =="
cp -f build/gopt_cli.exe build/gopt_gui.exe build/gopt_verify.exe release/GameOptimizer/
cp -f build/GameOptimizer-setup.exe release/
powershell -NoProfile -Command "Compress-Archive -Path 'release/GameOptimizer' -DestinationPath 'release/GameOptimizer-portable.zip' -Force"

echo "== 产物 =="
ls -la build/*.exe release/GameOptimizer-portable.zip release/GameOptimizer-setup.exe
