// GameOptimizer GUI 端到端驱动测试（真实进程 + 真实 GUI 按钮）
// 流程：造假 cs2.exe -> 启动 GUI -> 选 CS2 -> 应用优化 -> 验证优先级/亲和性 -> 回滚 -> 验证恢复
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class Win {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string title);
  [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr h, int id);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr GetClassNameW(IntPtr h, StringBuilder b, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr GetWindowTextW(IntPtr h, StringBuilder b, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern bool SetWindowTextW(IntPtr h, string t);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetWindowLongPtr(IntPtr h, int idx);
  [DllImport("user32.dll")] public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, EnumProc cb, IntPtr l);
  public const uint BM_CLICK=0x00F5, CB_SETCURSEL=0x014E, WM_GETTEXT=0x000D, WM_GETTEXTLENGTH=0x000E;
}
"@

# 1) 造假 cs2.exe（复制 cmd.exe），并挂起式运行一个真实进程（约 40s）
$tmp = "$env:TEMP\gopt_e2e"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
Copy-Item "$env:SystemRoot\System32\cmd.exe" "$tmp\cs2.exe" -Force
$fake = Start-Process -FilePath "$tmp\cs2.exe" -ArgumentList "/c","ping -n 40 127.0.0.1 >nul" -WindowStyle Hidden -PassThru
Write-Host "假 cs2 进程 pid=$($fake.Id)  存在=$(Get-Process -Id $fake.Id -ErrorAction SilentlyContinue -ne $null)"

# 2) 启动 GUI
$gui = Start-Process -FilePath "E:\deekseek  harness operrating area\GameOptimizer\build\gopt_gui.exe" -PassThru
Start-Sleep -Seconds 3
$hwnd = [Win]::FindWindowW("gopt_gui", $null)
if ($hwnd -eq [IntPtr]::Zero) { Write-Host "找不到 GUI 窗口"; exit 1 }
Write-Host "GUI 窗口 hwnd=$hwnd"
$apply = [Win]::GetDlgItem($hwnd, 101)
$rollback = [Win]::GetDlgItem($hwnd, 102)

# 找 ComboBox 与日志 Edit(多行)
$combo = [IntPtr]::Zero; $log = [IntPtr]::Zero
$cb = [Win+EnumProc]{ param($h,$l)
  $sb = New-Object System.Text.StringBuilder 64
  [Win]::GetClassNameW($h,$sb,64) | Out-Null
  if ($sb.ToString() -eq "ComboBox") { $script:comboH = $h }
  if ($sb.ToString() -eq "Edit") { $st = [Win]::GetWindowLongPtr($h, -16); if (($st -band 0x4) -ne 0) { $script:logH = $h } }
  return $true
}
[Win]::EnumChildWindows($hwnd, $cb, [IntPtr]::Zero) | Out-Null
$combo = $script:comboH; $log = $script:logH
Write-Host "combo=$combo  log=$log"

# 3) 选 CS2（列表下标 2），路径留空=attach；点应用优化
[Win]::SendMessageW($combo, [Win]::CB_SETCURSEL, [IntPtr]2, [IntPtr]::Zero) | Out-Null
Write-Host "=== 点击 应用优化 ==="
[Win]::SendMessageW($apply, [Win]::BM_CLICK, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
Start-Sleep -Seconds 2

function Get-LogText {
  $len = [Win]::SendMessageW($log, [Win]::WM_GETTEXTLENGTH, [IntPtr]::Zero, [IntPtr]::Zero).ToInt64()
  $sb = New-Object System.Text.StringBuilder ([int]$len + 2)
  [Win]::SendMessageW($log, [Win]::WM_GETTEXT, [IntPtr]($sb.Capacity), $sb) | Out-Null
  return $sb.ToString()
}
$t1 = Get-LogText
Write-Host "--- GUI 日志 ---"
Write-Host $t1
$p = Get-Process -Id $fake.Id -ErrorAction SilentlyContinue
if ($p) { Write-Host ("应用后 cs2 进程: 优先级={0}" -f $p.PriorityClass) }

# 4) 点击 回滚
Write-Host "=== 点击 回滚 ==="
[Win]::SendMessageW($rollback, [Win]::BM_CLICK, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
Start-Sleep -Seconds 2
$t2 = Get-LogText
Write-Host "--- GUI 日志(回滚后) ---"
Write-Host ($t2.Substring([Math]::Min($t1.Length, $t2.Length)))
$p2 = Get-Process -Id $fake.Id -ErrorAction SilentlyContinue
if ($p2) { Write-Host ("回滚后 cs2 进程: 优先级={0}" -f $p2.PriorityClass) }

# 清理
Stop-Process -Id $gui.Id -Force -ErrorAction SilentlyContinue
Stop-Process -Id $fake.Id -Force -ErrorAction SilentlyContinue
Remove-Item "$tmp\cs2.exe" -Force -ErrorAction SilentlyContinue
Write-Host "=== 完成 ==="