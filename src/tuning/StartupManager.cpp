#include "tuning/StartupManager.h"

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

#include <fstream>
#include <vector>

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

std::wstring Utf8ToWide(const std::string& s) {
    if (s.empty()) return {};
    const int len = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, nullptr, 0);
    if (len <= 1) return {};
    std::wstring w(static_cast<size_t>(len) - 1, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), -1, &w[0], len);
    return w;
}

struct KeyRef {
    const wchar_t* path;
    const wchar_t* hiveName;
};

const KeyRef kRunKeys[] = {
    {L"Software\\Microsoft\\Windows\\CurrentVersion\\Run", L"HKCU"},
    {L"Software\\Microsoft\\Windows\\CurrentVersion\\Run", L"HKLM"},
};

HKEY HiveOf(const std::wstring& hive) {
    return hive == L"HKLM" ? HKEY_LOCAL_MACHINE : HKEY_CURRENT_USER;
}

bool ReadValue(HKEY root, const wchar_t* path, const std::wstring& name,
               std::wstring* data, DWORD* type) {
    HKEY k = nullptr;
    if (RegOpenKeyExW(root, path, 0, KEY_READ, &k) != ERROR_SUCCESS) return false;
    DWORD size = 0;
    if (RegQueryValueExW(k, name.c_str(), nullptr, nullptr, nullptr, &size) != ERROR_SUCCESS) {
        RegCloseKey(k);
        return false;
    }
    std::vector<wchar_t> buf(size / sizeof(wchar_t) + 1, 0);
    if (RegQueryValueExW(k, name.c_str(), nullptr, type,
                         reinterpret_cast<BYTE*>(buf.data()), &size) != ERROR_SUCCESS) {
        RegCloseKey(k);
        return false;
    }
    *data = buf.data();
    RegCloseKey(k);
    return true;
}

}  // namespace

std::string StartupManager::DisabledName(const std::string& name) {
    return "[disabled] " + name;
}

std::string StartupManager::BackupPath() {
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
    return WideToUtf8(dir.c_str()) + "\\startup_backup.conf";
}

std::vector<StartupEntry> StartupManager::List() {
    std::vector<StartupEntry> out;
    for (const KeyRef& key : kRunKeys) {
        HKEY k = nullptr;
        if (RegOpenKeyExW(HiveOf(key.hiveName), key.path, 0, KEY_READ | KEY_WOW64_64KEY,
                          &k) != ERROR_SUCCESS) {
            continue;
        }
        DWORD idx = 0;
        for (;;) {
            wchar_t name[512] = {};
            DWORD nameLen = 512;
            DWORD type = 0;
            std::vector<BYTE> buf(4096);
            DWORD len = static_cast<DWORD>(buf.size());
            const LONG r = RegEnumValueW(k, idx, name, &nameLen, nullptr, &type, buf.data(), &len);
            if (r == ERROR_NO_MORE_ITEMS) break;
            if (r != ERROR_SUCCESS) {
                ++idx;
                continue;
            }
            StartupEntry e;
            e.hive = WideToUtf8(key.hiveName);
            e.name = WideToUtf8(std::wstring(name, nameLen).c_str());
            if (type == REG_SZ || type == REG_EXPAND_SZ) {
                e.value = WideToUtf8(reinterpret_cast<const wchar_t*>(buf.data()));
            } else {
                e.value = "<其它类型>";
            }
            out.push_back(e);
            ++idx;
        }
        RegCloseKey(k);
    }
    return out;
}

// 找到指定名的启动项并执行"命名迁移"（禁用=原→[disabled] name；启用=反向）
static bool MoveValue(const std::string& from, const std::string& to) {
    for (const KeyRef& key : kRunKeys) {
        std::wstring data;
        DWORD type = 0;
        if (!ReadValue(HiveOf(key.hiveName), key.path, Utf8ToWide(from), &data, &type)) continue;
        HKEY k = nullptr;
        if (RegOpenKeyExW(HiveOf(key.hiveName), key.path, 0, KEY_WRITE, &k) != ERROR_SUCCESS)
            return false;
        const bool ok = RegSetValueExW(k, Utf8ToWide(to).c_str(), 0, type,
                                       reinterpret_cast<const BYTE*>(data.c_str()),
                                       static_cast<DWORD>((data.size() + 1) * sizeof(wchar_t))) == ERROR_SUCCESS &&
                       RegDeleteValueW(k, Utf8ToWide(from).c_str()) == ERROR_SUCCESS;
        RegCloseKey(k);
        return ok;
    }
    return false;
}

bool StartupManager::Disable(const std::string& name) {
    const std::string disabled = DisabledName(name);
    // 先确认存在（任一 hive）
    bool found = false;
    for (const KeyRef& key : kRunKeys) {
        std::wstring data;
        DWORD type = 0;
        if (ReadValue(HiveOf(key.hiveName), key.path, Utf8ToWide(name), &data, &type)) {
            found = true;
            break;
        }
    }
    if (!found) return false;
    if (!MoveValue(name, disabled)) return false;
    std::ofstream b(BackupPath(), std::ios::app);
    b << "HKCU|" << name << '|' << disabled << "\n";
    return true;
}

bool StartupManager::Enable(const std::string& name) {
    const std::string disabled = DisabledName(name);
    bool found = false;
    for (const KeyRef& key : kRunKeys) {
        std::wstring data;
        DWORD type = 0;
        if (ReadValue(HiveOf(key.hiveName), key.path, Utf8ToWide(disabled), &data, &type)) {
            found = true;
            break;
        }
    }
    if (!found) return false;
    if (!MoveValue(disabled, name)) return false;
    // 删除备份行（该启动项的所有行）
    std::ifstream in(BackupPath());
    std::string line, all;
    while (std::getline(in, line)) {
        if (line.find(name + '|') == std::string::npos || line.find("|" + name + "|") == std::string::npos) {
            all += line + "\n";
        }
    }
    std::ofstream out(BackupPath(), std::ios::trunc);
    out << all;
    return true;
}

int StartupManager::RestoreAll() {
    std::ifstream in(BackupPath());
    std::string line;
    int restored = 0;
    while (std::getline(in, line)) {
        // 格式: HKCU|<name>|[disabled] <name>
        const size_t p2 = line.rfind('|');
        if (p2 == std::string::npos) continue;
        std::string disabled = line.substr(p2 + 1);
        std::string name = disabled;
        if (name.rfind("[disabled] ", 0) == 0) name = name.substr(11);
        if (MoveValue(disabled, name)) ++restored;
    }
    // 清空备份
    std::ofstream out(BackupPath(), std::ios::trunc);
    out.flush();
    return restored;
}

}  // namespace gopt
