#include <windows.h>
#include <shlobj.h>
#include <objbase.h>
#include <shellapi.h>

#include <cstdio>
#include <string>

// 三个内嵌安装文件（RCDATA）
enum { RID_CLI = 1, RID_VERIFY = 2, RID_README = 3 };

static std::wstring GetLocalAppData() {
    wchar_t b[MAX_PATH] = {};
    if (GetEnvironmentVariableW(L"LOCALAPPDATA", b, MAX_PATH) > 0) return b;
    GetCurrentDirectoryW(MAX_PATH, b);
    return b;
}

static std::wstring GetProgramsDir() {
    wchar_t b[MAX_PATH] = {};
    if (SUCCEEDED(SHGetFolderPathW(nullptr, CSIDL_PROGRAMS, nullptr, 0, b))) return b;
    return L"";
}

static std::wstring GetSelfPath() {
    wchar_t b[MAX_PATH] = {};
    GetModuleFileNameW(nullptr, b, MAX_PATH);
    return b;
}

static void WriteResourceToFile(HMODULE hMod, int id, const std::wstring& path) {
    HRSRC r = FindResourceW(hMod, MAKEINTRESOURCEW(id), (LPCWSTR)RT_RCDATA);
    if (!r) return;
    const DWORD sz = SizeofResource(hMod, r);
    HGLOBAL hg = LoadResource(hMod, r);
    if (!hg) return;
    void* p = LockResource(hg);
    HANDLE f = CreateFileW(path.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, 0, nullptr);
    if (f != INVALID_HANDLE_VALUE) {
        DWORD written = 0;
        WriteFile(f, p, sz, &written, nullptr);
        CloseHandle(f);
    }
}

static void MakeShortcut(const std::wstring& lnk, const std::wstring& target,
                         const std::wstring& workdir) {
    IShellLinkW* link = nullptr;
    if (SUCCEEDED(CoCreateInstance(CLSID_ShellLink, nullptr, CLSCTX_INPROC_SERVER,
                                   IID_IShellLinkW, reinterpret_cast<void**>(&link)))) {
        link->SetPath(target.c_str());
        if (!workdir.empty()) link->SetWorkingDirectory(workdir.c_str());
        IPersistFile* pf = nullptr;
        if (SUCCEEDED(link->QueryInterface(IID_IPersistFile,
                                           reinterpret_cast<void**>(&pf)))) {
            pf->Save(lnk.c_str(), TRUE);
            pf->Release();
        }
        link->Release();
    }
}

static void RegWriteString(HKEY root, const wchar_t* sub, const wchar_t* name,
                           const std::wstring& value) {
    HKEY k = nullptr;
    if (RegCreateKeyExW(root, sub, 0, nullptr, 0, KEY_WRITE, nullptr, &k, nullptr) ==
        ERROR_SUCCESS) {
        RegSetValueExW(k, name, 0, REG_SZ,
                       reinterpret_cast<const BYTE*>(value.c_str()),
                       static_cast<DWORD>((value.size() + 1) * sizeof(wchar_t)));
        RegCloseKey(k);
    }
}

static void RegDeleteKeyS(HKEY root, const wchar_t* sub) {
    RegDeleteKeyW(root, sub);
}

static int RunInstall() {
    CoInitialize(nullptr);
    const std::wstring dir = GetLocalAppData() + L"\\GameOptimizer";
    CreateDirectoryW(dir.c_str(), nullptr);
    const HMODULE hMod = GetModuleHandleW(nullptr);

    WriteResourceToFile(hMod, RID_CLI, dir + L"\\gopt_cli.exe");
    WriteResourceToFile(hMod, RID_VERIFY, dir + L"\\gopt_verify.exe");
    WriteResourceToFile(hMod, RID_README, dir + L"\\README.txt");
    WriteResourceToFile(hMod, 4, dir + L"\\gopt_gui.exe");

    // 拷贝自身作为卸载器
    const std::wstring uninst = dir + L"\\uninstall.exe";
    CopyFileW(GetSelfPath().c_str(), uninst.c_str(), FALSE);

    const std::wstring prog = GetProgramsDir() + L"\\GameOptimizer";
    CreateDirectoryW(prog.c_str(), nullptr);
    MakeShortcut(prog + L"\\GameOptimizer.lnk", dir + L"\\gopt_gui.exe", dir);
    MakeShortcut(prog + L"\\GameOptimizer CLI.lnk", dir + L"\\gopt_cli.exe", dir);
    MakeShortcut(prog + L"\\README.lnk", dir + L"\\README.txt", dir);
    MakeShortcut(prog + L"\\Uninstall.lnk", uninst, dir);

    RegWriteString(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\GameOptimizer",
                   L"DisplayName", L"GameOptimizer");
    RegWriteString(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\GameOptimizer",
                   L"DisplayVersion", L"1.0.0");
    RegWriteString(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\GameOptimizer",
                   L"Publisher", L"GameOptimizer");
    RegWriteString(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\GameOptimizer",
                   L"DisplayIcon", dir + L"\\gopt_cli.exe");
    RegWriteString(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\GameOptimizer",
                   L"UninstallString", L"\"" + uninst + L"\" --uninstall");

    CoUninitialize();
    MessageBoxW(nullptr,
                L"GameOptimizer v1.0.0 安装完成！\n"
                L"使用：开始菜单 -> GameOptimizer，或运行\n"
                L"%LOCALAPPDATA%\\GameOptimizer\\gopt_cli.exe\n",
                L"GameOptimizer", MB_OK | MB_ICONINFORMATION);
    return 0;
}

static int RunUninstall() {
    const std::wstring dir = GetLocalAppData() + L"\\GameOptimizer";
    const std::wstring prog = GetProgramsDir() + L"\\GameOptimizer";
    const std::wstring self = GetSelfPath();

    DeleteFileW((dir + L"\\gopt_cli.exe").c_str());
    DeleteFileW((dir + L"\\gopt_verify.exe").c_str());
    DeleteFileW((dir + L"\\gopt_gui.exe").c_str());
    DeleteFileW((dir + L"\\README.txt").c_str());
    DeleteFileW((prog + L"\\GameOptimizer.lnk").c_str());
    DeleteFileW((prog + L"\\GameOptimizer CLI.lnk").c_str());
    DeleteFileW((prog + L"\\README.lnk").c_str());
    DeleteFileW((prog + L"\\Uninstall.lnk").c_str());
    RemoveDirectoryW(prog.c_str());
    RegDeleteKeyS(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\GameOptimizer");
    RegDeleteKeyS(HKEY_CURRENT_USER, L"Software\\GameOptimizer");

    // 延迟删除自身与目录（批处理在进程退出后执行）
    wchar_t tmpDir[MAX_PATH] = {};
    GetTempPathW(MAX_PATH, tmpDir);
    const std::wstring bat = std::wstring(tmpDir) + L"\\gopt_uninstall_del.bat";
    char batText[1024];
    std::snprintf(batText, sizeof(batText),
                  "@echo off\r\n"
                  ":retry\r\n"
                  "ping -n 2 127.0.0.1 >nul\r\n"
                  "del /f /q \"%ls\" 2>nul\r\n"
                  "if exist \"%ls\" goto retry\r\n"
                  "rd /s /q \"%ls\" 2>nul\r\n"
                  "del /f /q \"%~f0\" 2>nul\r\n",
                  self.c_str(), self.c_str(), dir.c_str());
    HANDLE f = CreateFileW(bat.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, 0, nullptr);
    if (f != INVALID_HANDLE_VALUE) {
        DWORD written = 0;
        WriteFile(f, batText, static_cast<DWORD>(strlen(batText)), &written, nullptr);
        CloseHandle(f);
        ShellExecuteW(nullptr, L"open", bat.c_str(), nullptr, nullptr, SW_HIDE);
    }

    MessageBoxW(nullptr, L"GameOptimizer 已卸载。", L"GameOptimizer",
                MB_OK | MB_ICONINFORMATION);
    return 0;
}

int main() {
    int argcW = 0;
    wchar_t** argvW = CommandLineToArgvW(GetCommandLineW(), &argcW);
    bool uninstall = false;
    if (argvW && argcW > 1 && lstrcmpW(argvW[1], L"--uninstall") == 0) uninstall = true;
    if (argvW) LocalFree(argvW);
    return uninstall ? RunUninstall() : RunInstall();
}
