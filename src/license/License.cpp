#include "license/License.h"

#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0601
#endif
#ifndef WINVER
#define WINVER 0x0601
#endif
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif

#include <windows.h>

#include <algorithm>
#include <ctime>
#include <fstream>
#include <sstream>
#include <vector>

#include "license/sha256.h"

namespace gopt {

namespace {

std::string WideToUtf8(const wchar_t* wstr) {
    if (!wstr || !*wstr) return {};
    const int len = WideCharToMultiByte(CP_UTF8, 0, wstr, -1, nullptr, 0, nullptr, nullptr);
    if (len <= 1) return {};
    std::string s(static_cast<size_t>(len) - 1, '\0');
    WideCharToMultiByte(CP_UTF8, 0, wstr, -1, &s[0], len, nullptr, nullptr);
    return s;
}

std::string Trim(const std::string& s) {
    const size_t b = s.find_first_not_of(" \t\r\n");
    if (b == std::string::npos) return {};
    const size_t e = s.find_last_not_of(" \t\r\n");
    return s.substr(b, e - b + 1);
}

// 读 SMBIOS Type 2 (System Information) 的主板序列号
std::string ReadSmbiosBoardSerial() {
    const DWORD size = GetSystemFirmwareTable(0x424D5352u, 0, nullptr, 0);  // 'RSMB'
    if (size <= 8) return {};
    std::vector<BYTE> buf(size);
    if (GetSystemFirmwareTable(0x424D5352u, 0, buf.data(), size) != size) return {};
    uint32_t tableLen = 0;
    std::memcpy(&tableLen, buf.data() + 4, 4);
    tableLen = std::min<uint32_t>(tableLen, static_cast<uint32_t>(buf.size() - 8));
    const BYTE* table = buf.data() + 8;

    uint32_t off = 0;
    while (off + 4 <= tableLen) {
        const BYTE type = table[off];
        const BYTE len = table[off + 1];
        if (len < 4 || type == 0x7F) break;
        if (type == 2 && len >= 0x08) {
            const BYTE serialIndex = table[off + 0x07];  // Serial Number 字符串索引
            std::vector<std::string> strs;
            uint32_t pos = off + len;
            while (pos < tableLen && table[pos] != 0) {
                const char* start = reinterpret_cast<const char*>(table + pos);
                while (pos < tableLen && table[pos] != 0) ++pos;
                if (pos > static_cast<uint32_t>(start - reinterpret_cast<const char*>(table))) {
                    strs.emplace_back(start);
                }
                if (pos < tableLen) ++pos;  // 跳过 NUL
            }
            if (serialIndex > 0 && serialIndex <= strs.size()) return strs[serialIndex - 1];
            return {};
        }
        uint32_t pos = off + len;
        while (pos + 1 < tableLen && !(table[pos] == 0 && table[pos + 1] == 0)) {
            while (pos < tableLen && table[pos] != 0) ++pos;
            if (pos >= tableLen) break;
            ++pos;
        }
        if (pos + 1 >= tableLen) break;
        pos += 2;
        off = (pos + 1) & ~1u;
    }
    return {};
}

bool IsExpired(const std::string& expiry) {
    if (expiry.empty() || expiry == "permanent" || expiry == "0" || expiry == "forever")
        return false;
    std::string d = expiry;
    d.erase(std::remove(d.begin(), d.end(), '-'), d.end());
    if (d.size() != 8) return false;  // 无法解析视为永久
    const long exp = std::stol(d);
    const std::time_t t = std::time(nullptr);
    std::tm tmv{};
    localtime_s(&tmv, &t);
    const long today = (tmv.tm_year + 1900) * 10000L + (tmv.tm_mon + 1) * 100L + tmv.tm_mday;
    return today > exp;
}

bool SplitCode(const std::string& code, std::string* payload, std::string* sig) {
    const size_t dot = code.find('.');
    if (dot == std::string::npos) return false;
    *payload = sha::hex_decode(code.substr(0, dot));
    *sig = sha::hex_decode(code.substr(dot + 1));
    return !payload->empty() && !sig->empty();
}

// 解析 payload：u=<hash>&e=<ed>&x=<expiry>
bool ParsePayload(const std::string& payload, std::string* u, std::string* e, std::string* x) {
    std::istringstream is(payload);
    std::string kv;
    bool hasU = false, hasE = false;
    while (std::getline(is, kv, '&')) {
        const size_t eq = kv.find('=');
        if (eq == std::string::npos) continue;
        const std::string k = kv.substr(0, eq), v = kv.substr(eq + 1);
        if (k == "u") { *u = v; hasU = true; }
        else if (k == "e") { *e = v; hasE = true; }
        else if (k == "x") { *x = v; }
    }
    return hasU && hasE;
}

LicenseInfo VerifyCode(const std::string& code, const std::string& machHash,
                       const std::string& secret) {
    LicenseInfo info;
    std::string payload, sig;
    if (!SplitCode(code, &payload, &sig) || sha::hmac_sha256(secret, payload) != sig) {
        info.message = "授权码无效或签名校验失败";
        return info;
    }
    std::string u, e, x = "permanent";
    if (!ParsePayload(payload, &u, &e, &x)) {
        info.message = "授权码格式错误";
        return info;
    }
    if (u != machHash) {
        info.message = "授权码与本机不匹配（换机/更换硬件需重新激活）";
        return info;
    }
    if (IsExpired(x)) {
        info.message = "授权已过期（" + x + "）";
        return info;
    }
    info.valid = true;
    info.isPro = (e == "Pro" || e == "pro");
    info.edition = info.isPro ? "Pro" : "Free";
    info.expiry = x;
    info.message = "授权有效（" + info.edition + "）";
    return info;
}

}  // namespace

MachineFingerprint ComputeMachineFingerprint(const HardwareProfile& hw) {
    MachineFingerprint mf;
    mf.boardSerial = ReadSmbiosBoardSerial();
    std::ostringstream os;
    os << hw.cpuModel << "|" << hw.physicalCores << "|" << hw.logicalCores
       << "|" << hw.systemRamMB << "|" << hw.gpuVendorId << "|" << hw.vramMB
       << "|" << mf.boardSerial;
    mf.raw = os.str();
    mf.hash = sha::hex(sha::sha256(mf.raw));
    return mf;
}

std::string License::DefaultLicensePath() {
    wchar_t base[MAX_PATH] = {};
    std::wstring dir;
    if (GetEnvironmentVariableW(L"LOCALAPPDATA", base, MAX_PATH) > 0) {
        dir = base;
    } else {
        GetCurrentDirectoryW(MAX_PATH, base);
        dir = base;
    }
    dir += L"\\GameOptimizer";
    CreateDirectoryW(dir.c_str(), nullptr);
    return WideToUtf8(dir.c_str()) + "\\license.dat";
}

std::string License::Generate(const std::string& machineHash, const std::string& edition,
                              const std::string& expiry, const std::string& secret) {
    const std::string payload = "u=" + machineHash + "&e=" + edition + "&x=" + expiry;
    const std::string sig = sha::hmac_sha256(secret, payload);
    // 十六进制编码：精确无歧义，避免手写 base64 查表误差
    return sha::hex(payload) + "." + sha::hex(sig);
}

LicenseInfo License::Check(const HardwareProfile& hw, const std::string& path) {
    const std::string file = path.empty() ? DefaultLicensePath() : path;
    std::ifstream in(file);
    std::string code;
    std::getline(in, code);
    code = Trim(code);
    if (code.empty()) {
        LicenseInfo freeInfo;
        freeInfo.edition = "Free";
        freeInfo.message = "未安装授权（免费版）";
        return freeInfo;
    }
    const std::string machHash = ComputeMachineFingerprint(hw).hash;
    return VerifyCode(code, machHash, kSecret);
}

LicenseInfo License::Activate(const std::string& code, const HardwareProfile& hw,
                              const std::string& path) {
    const std::string machHash = ComputeMachineFingerprint(hw).hash;
    LicenseInfo info = VerifyCode(Trim(code), machHash, kSecret);
    if (info.valid) {
        const std::string file = path.empty() ? DefaultLicensePath() : path;
        std::ofstream out(file, std::ios::trunc);
        out << Trim(code) << "\n";
        info.message += "（已写入授权文件）";
    }
    return info;
}

}  // namespace gopt
