//! 进程级操作：优先级、亲和性、工作集、进程枚举（kernel32 + ToolHelp）。

use windows::core::PWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::SystemInformation::GROUP_AFFINITY;
use windows::Win32::System::Threading::{
    GetPriorityClass, GetProcessAffinityMask, GetProcessGroupAffinity, GetProcessWorkingSetSize,
    OpenProcess, OpenThread, QueryFullProcessImageNameW, SetPriorityClass, SetProcessAffinityMask,
    SetProcessWorkingSetSize, SetThreadGroupAffinity, PROCESS_ACCESS_RIGHTS,
    PROCESS_CREATION_FLAGS, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_INFORMATION, PROCESS_SET_QUOTA, THREAD_QUERY_INFORMATION, THREAD_SET_INFORMATION,
};

use super::handle::{OwnedHandle, ProcessHandle};
use super::{error_from, from_wide, last_error};
use crate::affinity::{AffinityApplied, AffinityInfo, AffinityMethod, AffinityPlan};
use crate::error::{HalError, HalResult};
use crate::types::{PriorityClass, ProcessInfo, WorkingSetLimits};

/// 打开目标进程；`pid == 当前进程` 时返回伪句柄（不关闭、无需权限）。
fn open_process(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> HalResult<ProcessHandle> {
    if pid == 0 {
        return Err(HalError::not_found(
            "OpenProcess",
            "pid 0 is the system idle process and is not a valid target",
        ));
    }
    if pid == std::process::id() {
        return Ok(ProcessHandle::PseudoCurrent);
    }
    let handle = unsafe { OpenProcess(access, false, pid) }.map_err(|error| {
        let mapped = error_from("OpenProcess", &error, format!("target pid {pid}"));
        // OpenProcess 对不存在的 PID 返回 ERROR_INVALID_PARAMETER(87)；
        // 与"参数非法"区分开，让调用方可以直接按 NotFound 处理（进程已退出）。
        if matches!(mapped.win32_code(), Some(2 | 3 | 87 | 1168)) {
            mapped.as_not_found()
        } else {
            mapped
        }
    })?;
    Ok(ProcessHandle::owned(handle))
}

/// 打开目标线程（用于 >64 逻辑核时的逐线程组亲和性）。
fn open_thread(tid: u32) -> HalResult<OwnedHandle> {
    let handle = unsafe {
        OpenThread(
            THREAD_SET_INFORMATION | THREAD_QUERY_INFORMATION,
            false,
            tid,
        )
    }
    .map_err(|error| error_from("OpenThread", &error, format!("thread {tid}")))?;
    Ok(OwnedHandle::new(handle))
}

/// 目标进程所在的处理器组（Win7+；查询失败时退化为组 0）。
fn process_group(handle: HANDLE) -> Option<u16> {
    let mut count: u16 = 0;
    unsafe {
        let _ = GetProcessGroupAffinity(handle, &mut count, core::ptr::null_mut());
    }
    if count == 0 {
        return None;
    }
    let mut groups = vec![0u16; count as usize];
    let ok = unsafe { GetProcessGroupAffinity(handle, &mut count, groups.as_mut_ptr()) };
    if ok.as_bool() {
        groups.first().copied()
    } else {
        None
    }
}

/// 目标进程的全部线程 ID（ToolHelp 快照）。
fn thread_ids_of(pid: u32) -> HalResult<Vec<u32>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
        .map_err(|error| error_from("CreateToolhelp32Snapshot", &error, "TH32CS_SNAPTHREAD"))?;
    let snapshot = OwnedHandle::new(snapshot);

    let mut entry = THREADENTRY32 {
        dwSize: core::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut ids = Vec::new();
    unsafe {
        if Thread32First(snapshot.raw(), &mut entry).is_err() {
            return Ok(ids);
        }
        loop {
            if entry.th32OwnerProcessID == pid {
                ids.push(entry.th32ThreadID);
            }
            if Thread32Next(snapshot.raw(), &mut entry).is_err() {
                break;
            }
        }
    }
    Ok(ids)
}

/// 进程映像完整路径；受保护进程会失败，返回 `None`（不视为错误）。
fn process_image_path(pid: u32) -> Option<String> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let mut buffer = vec![0u16; 32 * 1024];
    let mut size = buffer.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            process.raw(),
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    match result {
        Ok(()) => Some(from_wide(&buffer[..size as usize])),
        Err(_) => None,
    }
}

/// 读取目标进程优先级类。
pub(crate) fn get_priority(pid: u32) -> HalResult<PriorityClass> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let raw = unsafe { GetPriorityClass(process.raw()) };
    if raw == 0 {
        return Err(last_error("GetPriorityClass").with_context(format!("target pid {pid}")));
    }
    PriorityClass::from_raw(raw).map_err(|error| {
        error.with_context(format!(
            "target pid {pid} currently runs with an unmanaged priority class"
        ))
    })
}

/// 设置目标进程优先级，返回设置前的优先级。
pub(crate) fn set_priority(pid: u32, class: PriorityClass) -> HalResult<PriorityClass> {
    // 先读旧值：既作为回滚依据，也顺带确认目标进程存在/可访问。
    let previous = get_priority(pid)?;
    let process = open_process(pid, PROCESS_SET_INFORMATION)?;
    unsafe { SetPriorityClass(process.raw(), PROCESS_CREATION_FLAGS(class.raw())) }.map_err(
        |error| {
            error_from(
                "SetPriorityClass",
                &error,
                format!("pid {pid}, class {}", class.as_str()),
            )
        },
    )?;
    Ok(previous)
}

/// 读取目标进程亲和性快照。
pub(crate) fn get_affinity(pid: u32) -> HalResult<AffinityInfo> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut process_mask: usize = 0;
    let mut system_mask: usize = 0;
    unsafe { GetProcessAffinityMask(process.raw(), &mut process_mask, &mut system_mask) }
        .map_err(|error| error_from("GetProcessAffinityMask", &error, format!("pid {pid}")))?;

    Ok(AffinityInfo {
        pid,
        group: process_group(process.raw()).unwrap_or(0) as u32,
        process_mask: process_mask as u64,
        system_mask: system_mask as u64,
        total_logical: super::system::logical_processor_count(),
    })
}

/// 应用亲和性计划。
///
/// * `group == 0` 的单组计划 → `SetProcessAffinityMask`（进程主组，常规路径）；
/// * `group != 0` 的单组计划 → 逐线程 `SetThreadGroupAffinity`（>64 逻辑核机器的唯一手段）；
/// * 多组计划 → `Unsupported`，要求调用方先用 [`AffinityPlan::per_group`] 拆分
///   （一个线程只能属于一个处理器组，"整体绑定到多组"在 Win32 里没有对应语义）。
pub(crate) fn set_affinity(pid: u32, plan: &AffinityPlan) -> HalResult<AffinityApplied> {
    if !plan.is_single_group() {
        return Err(HalError::unsupported(
            "SetProcessAffinityMask",
            format!(
                "the plan spans {} processor groups ({}); apply one group at a time via \
                 AffinityPlan::per_group()",
                plan.requests().len(),
                plan
            ),
        ));
    }
    let request = plan.requests().first().copied().ok_or_else(|| {
        HalError::invalid_argument(
            "SetProcessAffinityMask",
            "an affinity plan needs at least one processor group",
        )
    })?;

    if request.group() == 0 {
        let process = open_process(pid, PROCESS_SET_INFORMATION)?;
        unsafe { SetProcessAffinityMask(process.raw(), request.mask() as usize) }.map_err(|error| {
            error_from(
                "SetProcessAffinityMask",
                &error,
                format!(
                    "pid {pid}, mask {:#018x} (the mask must be a subset of the processors active in \
                     the process's group)",
                    request.mask()
                ),
            )
        })?;
        return Ok(AffinityApplied {
            plan: plan.clone(),
            method: AffinityMethod::ProcessAffinityMask,
            threads_updated: 0,
        });
    }

    let affinity = GROUP_AFFINITY {
        Mask: request.mask() as usize,
        Group: request.group() as u16,
        Reserved: [0; 3],
    };
    let thread_ids = thread_ids_of(pid)?;
    if thread_ids.is_empty() {
        return Err(HalError::not_found(
            "Thread32First",
            format!("process {pid} has no visible threads"),
        ));
    }

    let mut updated: u32 = 0;
    let mut first_error: Option<HalError> = None;
    for tid in thread_ids {
        let thread = match open_thread(tid) {
            Ok(thread) => thread,
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                continue;
            }
        };
        let ok = unsafe { SetThreadGroupAffinity(thread.raw(), &affinity, None) };
        if ok.as_bool() {
            updated += 1;
        } else if first_error.is_none() {
            first_error =
                Some(last_error("SetThreadGroupAffinity").with_context(format!("thread {tid}")));
        }
    }

    if updated == 0 {
        return Err(first_error.unwrap_or_else(|| {
            HalError::internal(
                "SetThreadGroupAffinity",
                format!("process {pid} has no thread that accepted the group affinity"),
            )
        }));
    }
    Ok(AffinityApplied {
        plan: plan.clone(),
        method: AffinityMethod::ThreadGroupAffinity,
        threads_updated: updated,
    })
}

/// 读取进程工作集上下限（官方 `GetProcessWorkingSetSize`，只读）。
///
/// 返回值忠实反映系统：`min_bytes` 可能为 0（此时无法经 HAL 写回，见
/// [`WorkingSetLimits::is_restorable`]），`max_bytes` 可能是很大的真实值。
pub(crate) fn get_working_set(pid: u32) -> HalResult<WorkingSetLimits> {
    let process = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut minimum: usize = 0;
    let mut maximum: usize = 0;
    unsafe { GetProcessWorkingSetSize(process.raw(), &mut minimum, &mut maximum) }
        .map_err(|error| error_from("GetProcessWorkingSetSize", &error, format!("pid {pid}")))?;
    WorkingSetLimits::observed(minimum as u64, maximum as u64).map_err(|error| {
        error.with_context(format!(
            "pid {pid} reported working-set limits that cannot be represented \
             (min {minimum} bytes, max {maximum} bytes)"
        ))
    })
}

/// 设置进程工作集上下限。
pub(crate) fn set_working_set(pid: u32, limits: WorkingSetLimits) -> HalResult<()> {
    let limits = limits.normalized();
    let process = open_process(pid, PROCESS_SET_QUOTA)?;
    unsafe {
        SetProcessWorkingSetSize(
            process.raw(),
            limits.min_bytes as usize,
            limits.max_bytes as usize,
        )
    }
    .map_err(|error| {
        error_from(
            "SetProcessWorkingSetSize",
            &error,
            format!(
                "pid {pid}, min {} MiB, max {} MiB",
                limits.min_bytes / (1024 * 1024),
                limits.max_bytes / (1024 * 1024)
            ),
        )
    })
}

/// 枚举运行中的进程（按 PID 升序；受保护进程的路径为 `None`）。
pub(crate) fn list_processes() -> HalResult<Vec<ProcessInfo>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|error| error_from("CreateToolhelp32Snapshot", &error, "TH32CS_SNAPPROCESS"))?;
    let snapshot = OwnedHandle::new(snapshot);

    let mut entry = PROCESSENTRY32W {
        dwSize: core::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut processes = Vec::new();
    unsafe {
        if Process32FirstW(snapshot.raw(), &mut entry).is_err() {
            return Err(last_error("Process32FirstW"));
        }
        loop {
            let pid = entry.th32ProcessID;
            processes.push(ProcessInfo {
                pid,
                parent_pid: entry.th32ParentProcessID,
                name: from_wide(&entry.szExeFile),
                exe_path: process_image_path(pid),
                thread_count: entry.cntThreads,
            });
            if Process32NextW(snapshot.raw(), &mut entry).is_err() {
                break;
            }
        }
    }
    processes.sort_by_key(|process| process.pid);
    Ok(processes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::HalErrorKind;

    #[test]
    fn missing_process_is_reported_as_not_found() {
        // 取一个几乎不可能存在的 PID；真机上 OpenProcess 返回 ERROR_INVALID_PARAMETER(87)，
        // HAL 必须把它归类成"目标进程不存在"而不是"参数非法"。
        let err = get_priority(0xffff_fff0).expect_err("no such process");
        assert_eq!(err.kind(), HalErrorKind::NotFound, "{err}");
    }

    #[test]
    fn pid_zero_is_rejected_without_touching_the_system() {
        let err = get_priority(0).expect_err("pid 0 is not a target");
        assert_eq!(err.kind(), HalErrorKind::NotFound);
        assert_eq!(err.operation(), "OpenProcess");
    }

    #[test]
    fn current_process_priority_is_readable_and_whitelisted() {
        let class = get_priority(std::process::id()).expect("read own priority");
        assert!(PriorityClass::ALL.contains(&class), "{class}");
        let affinity = get_affinity(std::process::id()).expect("read own affinity");
        assert!(affinity.process_mask != 0);
        assert!(affinity.total_logical > 0);
    }

    #[test]
    fn multi_group_plan_is_rejected_before_touching_the_system() {
        // 96 逻辑核的计划在真机上跨组；无论本机有多少核，多组计划都必须被拒绝（不猜、不降级）。
        let plan = AffinityPlan::reserve_last_n_cores(96, 4).expect("plan");
        let err = set_affinity(std::process::id(), &plan).expect_err("cross-group unsupported");
        assert_eq!(err.kind(), HalErrorKind::Unsupported);
    }

    #[test]
    fn process_listing_contains_this_process() {
        let processes = list_processes().expect("enumerate");
        assert!(!processes.is_empty());
        let me = processes
            .iter()
            .find(|process| process.pid == std::process::id())
            .expect("current process must be listed");
        assert!(me.exe_path.is_some(), "{me:?}");
        assert!(me.thread_count >= 1);
    }

    #[test]
    fn own_working_set_is_readable() {
        // 成功路径：官方 GetProcessWorkingSetSize 必须给自己一个自洽的值。
        let limits = get_working_set(std::process::id()).expect("read own working set");
        assert!(limits.max_bytes >= limits.min_bytes, "{limits:?}");
        assert!(limits.max_bytes > 0, "{limits:?}");
        // 真机上默认下限通常非 0；若系统报告 0，也必须如实反映（不可写回）。
        if limits.min_bytes == 0 {
            assert!(!limits.is_restorable());
        }
    }

    #[test]
    fn working_set_read_rejects_missing_and_invalid_targets() {
        // 失败路径 1：目标进程不存在 ⇒ NotFound（与 get_priority 同一分类）。
        let err = get_working_set(0xffff_fff0).expect_err("no such process");
        assert_eq!(err.kind(), HalErrorKind::NotFound, "{err}");

        // 失败路径 2：pid 0 在接触系统之前就被拒绝。
        let err = get_working_set(0).expect_err("pid 0 is not a target");
        assert_eq!(err.kind(), HalErrorKind::NotFound);
        assert_eq!(err.operation(), "OpenProcess");
    }

    #[test]
    fn working_set_round_trip_on_the_current_process() {
        // 读 → 写 → 读回 → 还原 → 读回：证明读路径不是在"自说自话"。
        // 只针对本进程，且无论中间哪一步失败都会在结尾尝试还原。
        let before = get_working_set(std::process::id()).expect("read before");
        assert!(
            before.is_restorable(),
            "this machine reports min=0, the round-trip test cannot restore exactly: {before:?}"
        );

        let target = WorkingSetLimits::from_mb(64, 128).expect("target limits");
        set_working_set(std::process::id(), target).expect("set working set");
        let read_back = get_working_set(std::process::id()).expect("read back");

        let restore = set_working_set(std::process::id(), before);
        let restored = get_working_set(std::process::id());
        restore.expect("restore the original limits");

        assert_eq!(read_back.min_bytes, target.min_bytes, "{read_back:?}");
        assert_eq!(read_back.max_bytes, target.max_bytes, "{read_back:?}");
        let restored = restored.expect("read after restore");
        assert_eq!(restored, before, "the original limits must come back");
    }
}
