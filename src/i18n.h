#pragma once
// 轻量国际化：中文/英文运行时切换（GUI 下拉 / CLI --lang）
namespace gopt {

enum class Lang { Zh, En };

inline Lang& CurrentLang() {
    static Lang l = Lang::Zh;
    return l;
}
inline void SetLang(Lang l) { CurrentLang() = l; }

// 按当前语言返回字符串
inline const char* T(const char* zh, const char* en) {
    return CurrentLang() == Lang::En ? en : zh;
}

}  // namespace gopt
