// 真实端到端验证：
//   默认模式：单进程内  快照 → 应用 → 回滚 → 恢复校验
//   save     ：生成持久子进程，快照(写入磁盘) + 应用 HIGH/亲和性，然后退出（子进程保持优化、快照已持久化）
//   rollback <pid>：全新进程，从磁盘加载快照 → 回滚 → 读回子进程真实状态校验恢复
#include <cstdio>
#include <string>

#include "hal/HAL.h"
#include "preset/GamePreset.h"
#include "rollback/SecurityRollback.h"

namespace {

bool SpawnChild(const wchar_t* cmdLine, HANDLE* outProcess) {
    STARTUPINFOW si{};
    si.cb = sizeof(si);
    PROCESS_INFORMATION pi{};
    std::wstring cmd(cmdLine);
    if (!CreateProcessW(nullptr, &cmd[0], nullptr, nullptr, FALSE, CREATE_NO_WINDOW,
                        nullptr, nullptr, &si, &pi)) {
        return false;
    }
    *outProcess = pi.hProcess;
    CloseHandle(pi.hThread);
    return true;
}

void ReadState(HANDLE h, DWORD* pri, DWORD_PTR* mask) {
    *pri = GetPriorityClass(h);
    DWORD_PTR sys = 0;
    GetProcessAffinityMask(h, mask, &sys);
}

}  // namespace

int main(int argc, char** argv) {
    const std::string mode = (argc >= 2) ? argv[1] : "single";

    // ---------- save：生成子进程 → 快照(持久化) → 应用 → 退出 ----------
    if (mode == "save") {
        HANDLE child = nullptr;
        if (!SpawnChild(L"cmd.exe /c ping -n 60 127.0.0.1 >nul 2>&1", &child)) {
            std::puts("FAIL: 无法启动子进程");
            return 1;
        }
        const DWORD pid = GetProcessId(child);
        DWORD pri = 0;
        DWORD_PTR mask = 0;
        ReadState(child, &pri, &mask);
        std::printf("SAVE 子进程 pid=%lu  基线 pri=0x%lx mask=0x%llx\n",
                    static_cast<unsigned long>(pid), static_cast<unsigned long>(pri),
                    static_cast<unsigned long long>(mask));

        gopt::SecurityRollback rb;
        rb.CreateSavePoint(pid, "持久化测试");
        std::printf("SAVE 快照数=%zu（已写入磁盘）\n", rb.SavePointCount());

        gopt::GamePreset preset;
        preset.processPriorityClass = HIGH_PRIORITY_CLASS;
        preset.cpuAffinityGroup = 0;
        preset.cpuAffinityMask = static_cast<uint64_t>(mask) & (static_cast<uint64_t>(mask) - 1);
        const gopt::SecurityRollback::ApplyReport rep = gopt::SecurityRollback::ApplyPreset(pid, preset);
        std::printf("SAVE 应用 success=%d failed=%d（子进程已优化，进程保留运行）\n",
                    rep.appliedCount, rep.failedCount);
        CloseHandle(child);
        return 0;  // 关键：此处退出，模拟 apply 命令结束（子进程仍优化、快照在磁盘）
    }

    // ---------- rollback <pid>：新进程从磁盘加载快照 → 回滚 → 校验 ----------
    if (mode == "rollback" && argc >= 3) {
        const DWORD pid = static_cast<DWORD>(std::stoul(argv[2]));
        HANDLE h = OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_INFORMATION,
                               FALSE, pid);
        if (!h) {
            std::puts("ROLLBACK FAIL: OpenProcess 失败");
            return 1;
        }
        gopt::SecurityRollback rb;
        const bool ok = rb.RollbackToLastSave();
        std::printf("ROLLBACK ok=%d 详情: %s\n", ok ? 1 : 0, rb.LastErrorText().c_str());

        DWORD pri = 0;
        DWORD_PTR mask = 0;
        ReadState(h, &pri, &mask);
        std::printf("ROLLBACK 恢复后 pri=0x%lx mask=0x%llx\n",
                    static_cast<unsigned long>(pri),
                    static_cast<unsigned long long>(mask));
        // 校验：恢复为 NORMAL(0x20) 且此前被清除的最低位置位（即亲和性回到含 CPU0 的完整掩码）
        const bool restored = (ok && pri == 0x20 && ((mask & 1) != 0));
        std::printf(restored ? "RESULT: PASS\n" : "RESULT: FAIL\n");
        CloseHandle(h);
        return restored ? 0 : 1;
    }

    // ---------- 默认：单进程完整测试 ----------
    HANDLE child = nullptr;
    if (!SpawnChild(L"cmd.exe /c ping -n 8 127.0.0.1 >nul 2>&1", &child)) {
        std::puts("FAIL: 无法启动子进程");
        return 1;
    }
    const DWORD pid = GetProcessId(child);
    std::printf("目标真实进程 pid=%lu\n", pid);

    DWORD basePri = 0;
    DWORD_PTR baseMask = 0;
    ReadState(child, &basePri, &baseMask);
    std::printf("基线   : priority=0x%lx  affinity=0x%llx\n",
                static_cast<unsigned long>(basePri), static_cast<unsigned long long>(baseMask));

    gopt::SecurityRollback rb;
    rb.CreateSavePoint(pid, "真实验证");
    std::printf("快照数 = %zu\n", rb.SavePointCount());

    gopt::GamePreset preset;
    preset.processPriorityClass = HIGH_PRIORITY_CLASS;
    preset.cpuAffinityGroup = 0;
    preset.cpuAffinityMask = static_cast<uint64_t>(baseMask) & (static_cast<uint64_t>(baseMask) - 1);
    const gopt::SecurityRollback::ApplyReport rep = gopt::SecurityRollback::ApplyPreset(pid, preset);
    std::printf("应用    : success=%d failed=%d\n", rep.appliedCount, rep.failedCount);

    DWORD afterPri = 0;
    DWORD_PTR afterMask = 0;
    ReadState(child, &afterPri, &afterMask);
    std::printf("应用后  : priority=0x%lx  affinity=0x%llx\n",
                static_cast<unsigned long>(afterPri), static_cast<unsigned long long>(afterMask));

    const bool rbOk = rb.RollbackToLastSave();
    std::printf("回滚 ok=%d 详情: %s\n", rbOk ? 1 : 0, rb.LastErrorText().c_str());
    DWORD restorePri = 0;
    DWORD_PTR restoreMask = 0;
    ReadState(child, &restorePri, &restoreMask);
    std::printf("恢复后  : priority=0x%lx  affinity=0x%llx\n",
                static_cast<unsigned long>(restorePri), static_cast<unsigned long long>(restoreMask));

    const bool appliedOk = (afterPri == HIGH_PRIORITY_CLASS && afterMask == preset.cpuAffinityMask);
    const bool restoredOk = (restorePri == basePri && restoreMask == baseMask);
    std::printf("校验    : 应用生效=%s  回滚恢复=%s\n", appliedOk ? "是" : "否", restoredOk ? "是" : "否");
    std::printf(appliedOk && restoredOk ? "RESULT: PASS\n" : "RESULT: FAIL\n");

    TerminateProcess(child, 0);
    CloseHandle(child);
    return (appliedOk && restoredOk) ? 0 : 1;
}
