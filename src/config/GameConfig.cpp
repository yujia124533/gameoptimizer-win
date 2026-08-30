#include "config/GameConfig.h"

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
#include <map>
#include <sstream>
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

std::map<int, GameLaunchConfig>& Store() {
    static std::map<int, GameLaunchConfig> s;
    static bool loaded = false;
    if (!loaded) {
        loaded = true;
        std::ifstream in(GameConfig::ConfigPath());
        // 转成文字文件再读（GameConfig::ConfigPath 也调 WideToUtf8，避免循环）
        std::string line;
        while (std::getline(in, line)) {
            std::istringstream is(line);
            std::string t;
            int idx = -1;
            GameLaunchConfig cfg;
            if (std::getline(is, t, '|')) idx = std::stoi(t);
            if (!std::getline(is, cfg.exePath, '|')) continue;
            if (!std::getline(is, cfg.args, '|')) continue;
            std::string flags;
            if (!std::getline(is, flags, '|')) continue;
            if (flags.size() >= 4) {
                cfg.optimizedOnLaunch = flags[0] == '1';
                cfg.powerScheme = flags[1] == '1';
                cfg.frameLatency = flags[2] == '1';
                cfg.workingSet = flags[3] == '1';
            }
            if (idx >= 0) s[idx] = cfg;
        }
    }
    return s;
}

void Persist() {
    std::ofstream out(GameConfig::ConfigPath(), std::ios::trunc);
    for (const auto& [idx, cfg] : Store()) {
        out << idx << '|' << cfg.exePath << '|' << cfg.args << '|'
            << (cfg.optimizedOnLaunch ? '1' : '0')
            << (cfg.powerScheme ? '1' : '0')
            << (cfg.frameLatency ? '1' : '0')
            << (cfg.workingSet ? '1' : '0') << "\n";
    }
}

}  // namespace

std::string GameConfig::ConfigPath() {
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
    return WideToUtf8(dir.c_str()) + "\\games.conf";
}

GameLaunchConfig GameConfig::Get(GameId id) {
    const auto& s = Store();
    const auto it = s.find(static_cast<int>(id));
    if (it != s.end()) return it->second;
    return GameLaunchConfig{};  // 默认值
}

bool GameConfig::Set(GameId id, const GameLaunchConfig& cfg) {
    Store()[static_cast<int>(id)] = cfg;
    Persist();
    return true;
}

bool GameConfig::Remove(GameId id) {
    Store().erase(static_cast<int>(id));
    Persist();
    return true;
}

}  // namespace gopt
