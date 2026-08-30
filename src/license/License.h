#pragma once

#include <string>

#include "hardware/HardwareProfile.h"

namespace gopt {

// 机器指纹（用于授权绑定：换机即失效）
struct MachineFingerprint {
    std::string raw;        // 原始拼接串（用于日志/调试）
    std::string hash;       // SHA-256 十六进制摘要
    std::string boardSerial;  // SMBIOS 主板序列号（识别换机）
};
MachineFingerprint ComputeMachineFingerprint(const HardwareProfile& hw);

// 授权信息
struct LicenseInfo {
    bool valid = false;      // 授权是否有效
    bool isPro = false;      // 是否 Pro 版
    std::string edition;     // "Free" / "Pro"
    std::string expiry;      // 到期日（空=永久）
    std::string message;     // 校验结果说明
};

// 授权管理：免费版 vs Pro 买断；机器指纹绑定 + HMAC 签名授权码。
// 说明：v1 采用编译期内置密钥做 HMAC 签名，能挡"随手转发"级传播（不追求强加密破解）。
class License {
public:
    static constexpr const char* kSecret = "GameOptimizer-Lic-2026-ChangeMe";

    // 校验当前机器的授权（默认读取 %LOCALAPPDATA%\\GameOptimizer\\license.dat）
    static LicenseInfo Check(const HardwareProfile& hw, const std::string& path = "");
    // 安装授权码（校验通过后写入磁盘），返回校验结果
    static LicenseInfo Activate(const std::string& code, const HardwareProfile& hw,
                                const std::string& path = "");
    // 开发者：为指定机器指纹生成授权码
    static std::string Generate(const std::string& machineHash, const std::string& edition,
                                const std::string& expiry, const std::string& secret = kSecret);

    // 默认授权文件路径
    static std::string DefaultLicensePath();
};

}  // namespace gopt
