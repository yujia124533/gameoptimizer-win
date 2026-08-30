#include "hardware/HardwareDetector.h"

#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0601  // Windows 7+：GetLogicalProcessorInformationEx / CheckInterfaceSupport
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
#include <dxgi.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <optional>
#include <vector>

namespace gopt {

namespace {

// ---------------- 通用小工具 ----------------

int CountBits(uint64_t v) {
    int n = 0;
    while (v) {
        v &= v - 1;
        ++n;
    }
    return n;
}

#if defined(_MSC_VER)
#include <intrin.h>
void CpuId(uint32_t leaf, uint32_t subleaf, uint32_t out[4]) {
    __cpuidex(reinterpret_cast<int*>(out), leaf, subleaf);
}
#elif defined(__MINGW32__) || defined(__GNUC__)
#include <cpuid.h>
void CpuId(uint32_t leaf, uint32_t subleaf, uint32_t out[4]) {
    __cpuid_count(leaf, subleaf, out[0], out[1], out[2], out[3]);
}
#else
#error "Unsupported compiler: need __cpuidex or __cpuid_count"
#endif

std::string WideToUtf8(const wchar_t* wstr) {
    if (!wstr || !*wstr) return {};
    const int len = WideCharToMultiByte(CP_UTF8, 0, wstr, -1, nullptr, 0, nullptr, nullptr);
    if (len <= 1) return {};
    std::string s(static_cast<size_t>(len) - 1, '\0');
    WideCharToMultiByte(CP_UTF8, 0, wstr, -1, &s[0], len, nullptr, nullptr);
    return s;
}

std::wstring RegReadString(HKEY root, const wchar_t* path, const wchar_t* name) {
    std::wstring result;
    HKEY key = nullptr;
    if (RegOpenKeyExW(root, path, 0, KEY_READ, &key) != ERROR_SUCCESS) return result;
    DWORD size = 0;
    if (RegQueryValueExW(key, name, nullptr, nullptr, nullptr, &size) == ERROR_SUCCESS && size > 1) {
        result.resize(size / sizeof(wchar_t));
        DWORD type = 0;
        if (RegQueryValueExW(key, name, nullptr, &type,
                             reinterpret_cast<BYTE*>(&result[0]), &size) == ERROR_SUCCESS) {
            while (!result.empty() && result.back() == L'\0') result.pop_back();
        } else {
            result.clear();
        }
    }
    RegCloseKey(key);
    return result;
}

uint32_t RegReadDword(HKEY root, const wchar_t* path, const wchar_t* name, uint32_t def) {
    HKEY key = nullptr;
    if (RegOpenKeyExW(root, path, 0, KEY_READ, &key) != ERROR_SUCCESS) return def;
    DWORD value = 0;
    DWORD type = 0;
    DWORD size = sizeof(value);
    if (RegQueryValueExW(key, name, nullptr, &type,
                         reinterpret_cast<BYTE*>(&value), &size) != ERROR_SUCCESS || type != REG_DWORD) {
        value = def;
    }
    RegCloseKey(key);
    return value;
}

// CPUID 品牌字符串（注册表读取失败时的兜底）
std::string CpuIdBrandString() {
    uint32_t regs[4] = {};
    CpuId(0x80000000u, 0, regs);
    if (regs[0] < 0x80000004u) return {};
    char brand[49] = {};
    for (uint32_t leaf = 0x80000002u; leaf <= 0x80000004u; ++leaf) {
        CpuId(leaf, 0, regs);
        std::memcpy(brand + (leaf - 0x80000002u) * 16, regs, 16);
    }
    std::string s(brand);
    const size_t b = s.find_first_not_of(" \t\r\n");
    if (b == std::string::npos) return {};
    const size_t e = s.find_last_not_of(" \t\r\n");
    return s.substr(b, e - b + 1);
}

// 当前进程令牌是否启用了指定特权（如 SeLockMemoryPrivilege）
bool HasPrivilegeEnabled(const wchar_t* privilegeName) {
    HANDLE token = nullptr;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)) return false;

    LUID target{};
    const bool haveTarget = LookupPrivilegeValueW(nullptr, privilegeName, &target) != FALSE;

    bool enabled = false;
    DWORD size = 0;
    GetTokenInformation(token, TokenPrivileges, nullptr, 0, &size);
    std::vector<BYTE> buf(size > 0 ? size : 1);
    if (GetTokenInformation(token, TokenPrivileges, buf.data(),
                            static_cast<DWORD>(buf.size()), &size)) {
        const auto* privs = reinterpret_cast<const TOKEN_PRIVILEGES*>(buf.data());
        for (DWORD i = 0; i < privs->PrivilegeCount; ++i) {
            const LUID_AND_ATTRIBUTES& la = privs->Privileges[i];
            if (haveTarget && la.Luid.HighPart == target.HighPart &&
                la.Luid.LowPart == target.LowPart && (la.Attributes & SE_PRIVILEGE_ENABLED)) {
                enabled = true;
                break;
            }
        }
    }
    CloseHandle(token);
    return enabled;
}

// ---------------- CPU 探测 ----------------

void DetectCpu(HardwareProfile& p) {
    static const wchar_t* kCpuReg =
        L"HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0";

    // 型号：注册表 ProcessorNameString 优先，CPUID 品牌字符串兜底
    p.cpuModel =
        WideToUtf8(RegReadString(HKEY_LOCAL_MACHINE, kCpuReg, L"ProcessorNameString").c_str());
    if (p.cpuModel.empty()) p.cpuModel = CpuIdBrandString();

    // 基础频率：注册表 ~MHz
    p.cpuBaseFreqMHz = RegReadDword(HKEY_LOCAL_MACHINE, kCpuReg, L"~MHz", 0);

    // 逻辑核数（兜底值；下面 Ex 遍历得到更精确结果时覆盖）
    SYSTEM_INFO si{};
    GetSystemInfo(&si);
    p.logicalCores = static_cast<int>(si.dwNumberOfProcessors);

    // 物理核 / 超线程 / 每核逻辑处理器掩码：GetLogicalProcessorInformationEx
    DWORD len = 0;
    GetLogicalProcessorInformationEx(RelationProcessorCore, nullptr, &len);
    std::vector<uint64_t> buf((len + 7) / 8);  // 8 字节对齐，满足结构体对齐要求
    if (len > 0 && GetLogicalProcessorInformationEx(
                       RelationProcessorCore,
                       reinterpret_cast<PSYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>(buf.data()),
                       &len)) {
        const size_t total = len;  // 实际写入的字节数
        size_t off = 0;
        int cores = 0;
        int logical = 0;
        while (off + sizeof(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX) <= total) {
            const auto* entry = reinterpret_cast<const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX*>(
                reinterpret_cast<const BYTE*>(buf.data()) + off);
            if (entry->Size == 0) break;  // 防异常数据死循环
            if (entry->Relationship == RelationProcessorCore) {
                const PROCESSOR_RELATIONSHIP& rel = entry->Processor;
                for (BYTE g = 0; g < rel.GroupCount; ++g) {
                    CoreLayout layout;
                    layout.group = rel.GroupMask[g].Group;
                    layout.affinity = static_cast<uint64_t>(rel.GroupMask[g].Mask);
                    logical += CountBits(layout.affinity);
                    p.coreLayout.push_back(layout);
                }
                ++cores;
            }
            off += entry->Size;
        }
        if (cores > 0) {
            p.physicalCores = cores;
            if (logical > 0) p.logicalCores = logical;
        }
    }

    if (p.physicalCores == 0) {
        // 兜底：Ex 不可用 / 失败时，按"无超线程"处理
        p.physicalCores = p.logicalCores;
        CoreLayout layout;
        layout.group = 0;
        layout.affinity =
            p.logicalCores >= 64 ? ~0ull : ((1ull << p.logicalCores) - 1);
        p.coreLayout.push_back(layout);
    }

    p.supportsHyperThreading = p.logicalCores > p.physicalCores;

    // 最大睿频：仅 Intel（CPUID leaf 0x16，Skylake+；AMD 不实现该 leaf）
    uint32_t regs[4] = {};
    CpuId(0, 0, regs);
    const bool isIntel =
        (regs[1] == 0x756e6547u && regs[2] == 0x6c65746eu && regs[3] == 0x49656e69u);
    if (isIntel && regs[0] >= 0x16u) {
        CpuId(0x16u, 0, regs);
        p.cpuMaxTurboMHz = regs[2];
    }
}

// ---------------- GPU 探测（DXGI） ----------------

// 手动给出 IID，避免依赖 __uuidof / dxguid 链接
constexpr GUID kIID_IDXGIFactory1 = {0x770aae78, 0xf26f, 0x4dba,
                                     {0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87}};
constexpr GUID kIID_IDXGIDevice = {0x54ec77fa, 0x1377, 0x44e6,
                                   {0x8c, 0x32, 0x88, 0xfd, 0x5f, 0x44, 0xc8, 0x4c}};

std::string GpuVendorName(uint32_t vendorId) {
    switch (vendorId) {
        case 0x10DE: return "NVIDIA";
        case 0x1002: return "AMD";
        case 0x8086: return "Intel";
        case 0x1414: return "Microsoft";
        default:     return "Unknown";
    }
}

void DetectGpu(HardwareProfile& p) {
    IDXGIFactory1* factory = nullptr;
    HRESULT hr = CreateDXGIFactory1(kIID_IDXGIFactory1, reinterpret_cast<void**>(&factory));
    if (FAILED(hr) || factory == nullptr) return;

    // 枚举全部适配器，选择显存最大的真实硬件适配器（跳过软件 / 基础渲染驱动）
    IDXGIAdapter1* bestAdapter = nullptr;
    DXGI_ADAPTER_DESC1 bestDesc{};
    uint64_t bestVram = 0;

    for (UINT i = 0;; ++i) {
        IDXGIAdapter1* adapter = nullptr;
        if (factory->EnumAdapters1(i, &adapter) == DXGI_ERROR_NOT_FOUND) break;
        if (adapter == nullptr) continue;

        DXGI_ADAPTER_DESC1 desc{};
        if (SUCCEEDED(adapter->GetDesc1(&desc))) {
            const bool isSoftware = (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) != 0;
            const bool isBasicRender = (desc.VendorId == 0x1414 && desc.DeviceId == 0x008C);
            if (!isSoftware && !isBasicRender && desc.DedicatedVideoMemory >= bestVram) {
                if (bestAdapter != nullptr) bestAdapter->Release();
                bestAdapter = adapter;
                bestDesc = desc;
                bestVram = desc.DedicatedVideoMemory;
                adapter = nullptr;  // 所有权转移给 bestAdapter
            }
        }
        if (adapter != nullptr) adapter->Release();
    }

    if (bestAdapter != nullptr) {
        p.gpuVendorId = bestDesc.VendorId;
        p.gpuVendor = GpuVendorName(bestDesc.VendorId);
        p.gpuModel = WideToUtf8(bestDesc.Description);
        p.vramMB = bestDesc.DedicatedVideoMemory / (1024ull * 1024ull);
        p.gpuIsHardware = true;

        // 驱动版本：CheckInterfaceSupport（仅 WDDM 驱动有效）
        LARGE_INTEGER ver{};
        if (SUCCEEDED(bestAdapter->CheckInterfaceSupport(kIID_IDXGIDevice, &ver))) {
            const uint32_t hi = static_cast<uint32_t>(ver.HighPart);
            const uint32_t lo = static_cast<uint32_t>(ver.LowPart);
            char buf[64] = {};
            std::snprintf(buf, sizeof(buf), "%u.%u.%u.%u",
                          (hi >> 16) & 0xFFFFu, hi & 0xFFFFu,
                          (lo >> 16) & 0xFFFFu, lo & 0xFFFFu);
            p.gpuDriverVersion = buf;
        }
        bestAdapter->Release();
    }
    factory->Release();
}

// ---------------- 内存探测 ----------------

void DetectMemory(HardwareProfile& p) {
    // 总量 / 可用
    MEMORYSTATUSEX ms{};
    ms.dwLength = sizeof(ms);
    if (GlobalMemoryStatusEx(&ms)) {
        p.systemRamMB = ms.ullTotalPhys / (1024ull * 1024ull);
        p.availableRamMB = ms.ullAvailPhys / (1024ull * 1024ull);
    }

    // 频率：SMBIOS Type 17（Memory Device）
    const DWORD size = GetSystemFirmwareTable(0x424D5352u, 0, nullptr, 0);  // 'RSMB'
    if (size > 8) {
        std::vector<BYTE> buf(size);
        if (GetSystemFirmwareTable(0x424D5352u, 0, buf.data(), size) == size) {
            uint32_t tableLen = 0;
            std::memcpy(&tableLen, buf.data() + 4, 4);  // RawSMBIOSData::Length
            const BYTE* table = buf.data() + 8;          // RawSMBIOSData::SMBIOSTableData
            tableLen = std::min<uint32_t>(tableLen, static_cast<uint32_t>(buf.size() - 8));

            uint32_t off = 0;
            while (off + 4 <= tableLen) {
                const BYTE type = table[off];
                const BYTE len = table[off + 1];
                if (len < 4 || type == 0x7F) break;  // 异常保护 / 表结束(127)

                if (type == 17) {
                    // Speed: 偏移 0x15（需 len>=0x17）；ConfiguredMemorySpeed: 偏移 0x20（需 len>=0x22）
                    uint16_t speed = 0;
                    uint16_t configured = 0;
                    if (len >= 0x17) std::memcpy(&speed, table + off + 0x15, 2);
                    if (len >= 0x22) std::memcpy(&configured, table + off + 0x20, 2);
                    const uint32_t freq = configured ? configured : speed;
                    if (freq > p.memoryFreqMHz) p.memoryFreqMHz = freq;  // 取最高
                }

                // 跳过字符串区（逐串直到双 NUL）
                uint32_t pos = off + len;
                while (pos + 1 < tableLen && !(table[pos] == 0 && table[pos + 1] == 0)) {
                    while (pos < tableLen && table[pos] != 0) ++pos;  // 串尾 NUL
                    if (pos >= tableLen) break;
                    ++pos;  // 越过串尾 NUL
                }
                if (pos + 1 >= tableLen) break;
                pos += 2;              // 越过双 NUL
                off = (pos + 1) & ~1u; // 结构按 2 字节对齐
            }
        }
    }

    // 大页可用性：当前令牌是否启用 SeLockMemoryPrivilege
    p.largePagesEnabled = HasPrivilegeEnabled(L"SeLockMemoryPrivilege");
}

// ---------------- 汇总 ----------------

HardwareProfile DetectFresh() {
    HardwareProfile p;
    DetectCpu(p);
    DetectGpu(p);
    DetectMemory(p);
    return p;
}

std::optional<HardwareProfile>& ProfileCache() {
    static std::optional<HardwareProfile> cache;
    return cache;
}

std::mutex& CacheMutex() {
    static std::mutex mtx;
    return mtx;
}

}  // namespace

// ---------------- 对外接口 ----------------

HardwareProfile HardwareDetector::Detect() {
    std::lock_guard<std::mutex> lock(CacheMutex());
    std::optional<HardwareProfile>& cache = ProfileCache();
    if (!cache.has_value()) cache = DetectFresh();
    return *cache;
}

void HardwareDetector::ResetCache() {
    std::lock_guard<std::mutex> lock(CacheMutex());
    ProfileCache().reset();
}

bool HardwareDetector::IsGpuFeatureSupported(GpuFeature feature) {
    const HardwareProfile p = Detect();
    switch (feature) {
        case GpuFeature::HardwareAdapter:
            return p.gpuIsHardware;
        case GpuFeature::MaxFrameLatency:
            // 驱动级帧延迟配置只有 NVIDIA（NVAPI DRS）/ AMD（ADL）提供
            return p.gpuIsHardware && (p.gpuVendorId == 0x10DE || p.gpuVendorId == 0x1002);
    }
    return false;
}

}  // namespace gopt
