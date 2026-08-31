#pragma once

#include <string>
#include <vector>

namespace gopt {

// Wise Care 365 风格：开机启动项管理（HKCU/HKLM Run 键）
// 安全做法：禁用=改名（加 "[disabled] " 前缀）并记录备份；可一键恢复全部。
struct StartupEntry {
    std::string hive;   // "HKCU" / "HKLM"
    std::string name;   // 值名
    std::string value;  // 值内容
};

class StartupManager {
public:
    // 列出全部 Run 启动项
    static std::vector<StartupEntry> List();

    // 禁用指定启动项（返回是否找到并处理）
    static bool Disable(const std::string& name);

    // 恢复指定的已禁用启动项
    static bool Enable(const std::string& name);

    // 恢复全部（本工具禁用的启动项）
    static int RestoreAll();

    static std::string BackupPath();

private:
    static std::string DisabledName(const std::string& name);
};

}  // namespace gopt
