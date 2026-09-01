cmake_minimum_required(VERSION 3.16)

project(GameOptimizer
    VERSION 0.1.0
    DESCRIPTION "Windows 游戏优化工具：硬件无感适配 + 预设策略 + 安全回滚"
    LANGUAGES CXX RC)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

# ---- 核心库（全部模块：硬件探测 / HAL / 预设 / 回滚 / 协调层 / 授权） ----
add_library(gameopt_core STATIC
    src/hardware/HardwareProfile.cpp
    src/hardware/HardwareDetector.cpp
    src/hal/HAL.cpp
    src/preset/GameOptimizationPreset.cpp
    src/rollback/SecurityRollback.cpp
    src/config/GameConfig.cpp
    src/tuning/SystemTuner.cpp
    src/tuning/StartupManager.cpp
    src/license/License.cpp
    src/core/AppCore.cpp
)

target_include_directories(gameopt_core PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/src)
target_link_libraries(gameopt_core PUBLIC dxgi advapi32 powrprof)

target_compile_definitions(gameopt_core PRIVATE UNICODE _UNICODE NOMINMAX WIN32_LEAN_AND_MEAN)

if(MSVC)
    # /utf-8：源码含中文注释与字符串字面量
    target_compile_options(gameopt_core PRIVATE /W4 /permissive- /utf-8)
else()
    target_compile_options(gameopt_core PRIVATE -Wall -Wextra)
endif()

# ---- CLI（含版本资源） ----
add_executable(gopt_cli tools/cli_main.cpp resources/resource.rc)
target_link_libraries(gopt_cli PRIVATE gameopt_core)

# ---- 原生 GUI（多面板界面，含独立版本资源） ----
add_executable(gopt_gui WIN32
    src/gui/gopt_gui.cpp
    resources/gui_resource.rc
)
target_link_libraries(gopt_gui PRIVATE gameopt_core user32 gdi32 comdlg32)
