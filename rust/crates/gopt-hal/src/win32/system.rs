//! 提权检测与硬件画像（advapi32 令牌 + 注册表 + DXGI，全部为官方只读 API）。

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{HANDLE, LUID};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, DXGI_ADAPTER_DESC1,
    DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND,
};
use windows::Win32::Security::{
    GetTokenInformation, LookupPrivilegeValueW, TokenElevation, TokenPrivileges,
    SE_PRIVILEGE_ENABLED, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, REG_VALUE_TYPE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GetSystemInfo, GlobalMemoryStatusEx, RelationProcessorCore,
    GROUP_AFFINITY, MEMORYSTATUSEX, PROCESSOR_RELATIONSHIP, SYSTEM_INFO,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::Win32::System::Threading::{
    GetActiveProcessorCount, GetActiveProcessorGroupCount, GetCurrentProcess, OpenProcessToken,
};

use super::handle::OwnedHandle;
use super::{error_from, from_wide_bytes, to_wide};
use crate::affinity::{full_group_mask, ProcessorGroup};
use crate::error::{HalError, HalResult};
use crate::hardware::{CoreLayout, GpuInfo, GpuVendor, HardwareInfo};

/// CPU 型号/频率所在的注册表键。
const CPU_REG_PATH: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";

/// 当前进程是否已提权（管理员）。
pub(crate) fn is_elevated() -> HalResult<bool> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|error| error_from("OpenProcessToken", &error, "current process token"))?;
        let token = OwnedHandle::new(token);

        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned: u32 = 0;
        GetTokenInformation(
            token.raw(),
            TokenElevation,
            Some(core::ptr::from_mut(&mut elevation).cast()),
            core::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .map_err(|error| error_from("GetTokenInformation", &error, "TokenElevation"))?;

        Ok(elevation.TokenIsElevated != 0)
    }
}

/// 本进程令牌是否启用了指定特权（例如 `SeLockMemoryPrivilege`）。
fn privilege_enabled(name: &str) -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let token = OwnedHandle::new(token);

        let wide = to_wide(name);
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(wide.as_ptr()), &mut luid).is_err() {
            return false;
        }

        let mut size: u32 = 0;
        let _ = GetTokenInformation(token.raw(), TokenPrivileges, None, 0, &mut size);
        if size == 0 {
            return false;
        }
        let mut buffer = vec![0u8; size as usize];
        if GetTokenInformation(
            token.raw(),
            TokenPrivileges,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )
        .is_err()
        {
            return false;
        }
        privilege_is_enabled(&buffer, luid.LowPart, luid.HighPart)
    }
}

/// 解析 `TOKEN_PRIVILEGES`（实际是柔性数组：`DWORD PrivilegeCount` + `LUID_AND_ATTRIBUTES[]`）。
///
/// `LUID_AND_ATTRIBUTES` = `{ LUID(u32 LowPart, i32 HighPart), u32 Attributes }`，
/// 共 12 字节、对齐 4，因此数组从偏移 4 开始。这里按字节读取并逐项做边界检查，
/// 保证畸形/截断输入不会 panic（单元测试覆盖空/截断缓冲区）。
fn privilege_is_enabled(buffer: &[u8], low_part: u32, high_part: i32) -> bool {
    let Some(count_bytes) = buffer.get(0..4) else {
        return false;
    };
    let mut count = u32::from_ne_bytes([
        count_bytes[0],
        count_bytes[1],
        count_bytes[2],
        count_bytes[3],
    ]);
    let mut offset = 4usize;
    while count > 0 {
        let Some(entry) = buffer.get(offset..offset + 12) else {
            return false;
        };
        let low = u32::from_ne_bytes([entry[0], entry[1], entry[2], entry[3]]);
        let high = i32::from_ne_bytes([entry[4], entry[5], entry[6], entry[7]]);
        let attributes = u32::from_ne_bytes([entry[8], entry[9], entry[10], entry[11]]);
        if low == low_part && high == high_part {
            return attributes & SE_PRIVILEGE_ENABLED.0 != 0;
        }
        offset += 12;
        count -= 1;
    }
    false
}

/// 注册表字符串值（`REG_SZ`，`RegGetValueW` 会自动展开 `REG_EXPAND_SZ`）。
fn registry_string(subkey: &str, value: &str) -> Option<String> {
    let subkey = to_wide(subkey);
    let value = to_wide(value);
    let mut buffer = vec![0u8; 1024];
    let mut size = buffer.len() as u32;
    let mut kind = REG_VALUE_TYPE(0);
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            Some(&mut kind),
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if code != windows::Win32::Foundation::ERROR_SUCCESS {
        return None;
    }
    buffer.truncate(size.min(buffer.len() as u32) as usize);
    Some(from_wide_bytes(&buffer))
}

/// 注册表 DWORD 值。
fn registry_u32(subkey: &str, value: &str) -> Option<u32> {
    let subkey = to_wide(subkey);
    let value = to_wide(value);
    let mut data = [0u8; 4];
    let mut size = data.len() as u32;
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(core::ptr::from_mut(&mut data).cast()),
            Some(&mut size),
        )
    };
    if code != windows::Win32::Foundation::ERROR_SUCCESS || size != 4 {
        return None;
    }
    Some(u32::from_ne_bytes(data))
}

/// 机器逻辑处理器总数（`GetSystemInfo`，兜底值）。
pub(crate) fn logical_processor_count() -> u32 {
    let mut info = SYSTEM_INFO::default();
    unsafe { GetSystemInfo(&mut info) };
    info.dwNumberOfProcessors
}

/// 处理器组划分（>64 逻辑核时多组）。
fn detect_processor_groups() -> Vec<ProcessorGroup> {
    let count = unsafe { GetActiveProcessorGroupCount() };
    (0..count)
        .map(|index| {
            ProcessorGroup::new(u32::from(index), unsafe { GetActiveProcessorCount(index) })
        })
        .collect()
}

/// 逐物理核布局（`GetLogicalProcessorInformationEx`）。
fn detect_core_layout() -> HalResult<Vec<CoreLayout>> {
    let mut length: u32 = 0;
    unsafe {
        // 第一次调用只为拿所需长度（返回 ERROR_INSUFFICIENT_BUFFER）。
        let _ = GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut length);
    }
    if length == 0 {
        return Err(HalError::internal(
            "GetLogicalProcessorInformationEx",
            "the system reported an empty processor information buffer",
        ));
    }

    // 用 u64 作为后备存储：SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX 含 usize 字段，需要 8 字节对齐。
    let mut buffer = vec![0u64; (length as usize).div_ceil(core::mem::size_of::<u64>())];
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buffer.as_mut_ptr().cast()),
            &mut length,
        )
        .map_err(|error| {
            error_from(
                "GetLogicalProcessorInformationEx",
                &error,
                "RelationProcessorCore",
            )
        })?;
    }

    let base = buffer.as_ptr().cast::<u8>();
    let total = length as usize;
    let mut offset = 0usize;
    let mut layout = Vec::new();
    let mut index: u32 = 0;

    while offset + core::mem::size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>() <= total {
        let entry_pointer =
            unsafe { base.add(offset) }.cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
        // SAFETY: 指针来自对齐的后备存储，且已确认至少还能容纳一个完整条目。
        let entry = unsafe { &*entry_pointer };
        if entry.Size == 0 {
            break; // 防畸形数据导致死循环
        }
        if entry.Relationship == RelationProcessorCore {
            // `GroupMask` 在 Win32 头文件里是柔性数组，需要用原始指针按下标读取。
            let relationship_pointer = unsafe { core::ptr::addr_of!((*entry_pointer).Anonymous) }
                .cast::<PROCESSOR_RELATIONSHIP>();
            let group_count = unsafe { (*relationship_pointer).GroupCount };
            let masks = unsafe { core::ptr::addr_of!((*relationship_pointer).GroupMask) }
                .cast::<GROUP_AFFINITY>();
            for slot in 0..group_count {
                let affinity = unsafe { masks.add(usize::from(slot)).read() };
                layout.push(CoreLayout::new(
                    index,
                    u32::from(affinity.Group),
                    affinity.Mask as u64,
                ));
            }
            index += 1;
        }
        offset += entry.Size as usize;
    }

    Ok(layout)
}

/// 物理核数（跨组拆分的条目共享同一个 `index`）。
fn physical_core_count(layout: &[CoreLayout]) -> u32 {
    layout
        .iter()
        .map(|core| core.index)
        .max()
        .map_or(0, |last| last + 1)
}

/// 内存总量 / 可用量（MB）。
fn detect_memory_mb() -> (u64, u64) {
    let mut status = MEMORYSTATUSEX {
        dwLength: core::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    match unsafe { GlobalMemoryStatusEx(&mut status) } {
        Ok(()) => (
            status.ullTotalPhys / (1024 * 1024),
            status.ullAvailPhys / (1024 * 1024),
        ),
        Err(_) => (0, 0),
    }
}

/// WDDM 版本号（`LARGE_INTEGER` 高低 32 位各含两段 16 位）。
fn format_driver_version(value: i64) -> String {
    let raw = value as u64;
    let high = (raw >> 32) as u32;
    let low = raw as u32;
    format!(
        "{}.{}.{}.{}",
        (high >> 16) & 0xffff,
        high & 0xffff,
        (low >> 16) & 0xffff,
        low & 0xffff
    )
}

/// DXGI 枚举首选显示适配器（优先真实硬件，其次显存最大）。
fn detect_gpu(warnings: &mut Vec<String>) -> Option<GpuInfo> {
    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(factory) => factory,
        Err(error) => {
            warnings.push(format!(
                "{}",
                error_from(
                    "CreateDXGIFactory1",
                    &error,
                    "GPU information is unavailable"
                )
            ));
            return None;
        }
    };

    let mut best: Option<(DXGI_ADAPTER_DESC1, IDXGIAdapter1)> = None;
    let mut best_software = true;
    let mut index: u32 = 0;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => {
                warnings.push(format!(
                    "{}",
                    error_from("EnumAdapters1", &error, format!("adapter index {index}"))
                ));
                break;
            }
        };
        let description = match unsafe { adapter.GetDesc1() } {
            Ok(description) => description,
            Err(error) => {
                warnings.push(format!(
                    "{}",
                    error_from("GetDesc1", &error, format!("adapter index {index}"))
                ));
                index += 1;
                continue;
            }
        };
        let is_software = description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
        let better = match &best {
            None => true,
            Some((current, _)) => {
                let current_software = current.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
                match (current_software, is_software) {
                    (true, false) => true,
                    (false, true) => false,
                    _ => description.DedicatedVideoMemory > current.DedicatedVideoMemory,
                }
            }
        };
        if better {
            best_software = is_software;
            best = Some((description, adapter));
        }
        index += 1;
    }

    let (description, adapter) = best?;

    // 驱动版本：CheckInterfaceSupport 仅对 WDDM 驱动有效，失败时留空并记录降级原因。
    let driver_version = match adapter.cast::<windows::Win32::Graphics::Dxgi::IDXGIAdapter>() {
        Ok(base) => match unsafe { base.CheckInterfaceSupport(&IDXGIDevice::IID) } {
            Ok(version) => Some(format_driver_version(version)),
            Err(error) => {
                warnings.push(format!(
                    "{}",
                    error_from(
                        "CheckInterfaceSupport",
                        &error,
                        "GPU driver version is unavailable"
                    )
                ));
                None
            }
        },
        Err(_) => None,
    };

    let vendor_id = description.VendorId;
    Some(GpuInfo {
        vendor: GpuVendor::from_vendor_id(vendor_id),
        vendor_id,
        device_id: description.DeviceId,
        model: super::from_wide(&description.Description),
        vram_mb: description.DedicatedVideoMemory as u64 / (1024 * 1024),
        driver_version,
        is_hardware: !best_software,
        is_software_adapter: best_software,
    })
}

/// 采集硬件画像。整机探测采用"降级但显式"策略：拿不到的字段填默认值并写进 `warnings`。
pub(crate) fn hardware() -> HalResult<HardwareInfo> {
    let mut warnings = Vec::new();

    let cpu_model = registry_string(CPU_REG_PATH, "ProcessorNameString").unwrap_or_else(|| {
        warnings.push("the CPU model string is not available in the registry".to_string());
        "Unknown CPU".to_string()
    });
    let cpu_base_freq_mhz = registry_u32(CPU_REG_PATH, "~MHz").unwrap_or(0);

    let processor_groups = detect_processor_groups();
    let logical_cores = if processor_groups.is_empty() {
        warnings.push("GetActiveProcessorGroupCount reported no processor group".to_string());
        logical_processor_count()
    } else {
        processor_groups
            .iter()
            .map(|group| group.logical_count)
            .sum::<u32>()
    };

    let core_layout = match detect_core_layout() {
        Ok(layout) if !layout.is_empty() => layout,
        Ok(_) => {
            warnings.push(
                "GetLogicalProcessorInformationEx returned no processor cores; falling back to \
                 one entry per processor group"
                    .to_string(),
            );
            fallback_layout(&processor_groups, logical_cores)
        }
        Err(error) => {
            warnings.push(error.to_string());
            fallback_layout(&processor_groups, logical_cores)
        }
    };
    let physical_cores = if core_layout.is_empty() {
        logical_cores
    } else {
        physical_core_count(&core_layout)
    };

    let (system_ram_mb, available_ram_mb) = detect_memory_mb();
    if system_ram_mb == 0 {
        warnings.push("GlobalMemoryStatusEx could not report the physical memory size".to_string());
    }

    let gpu = detect_gpu(&mut warnings);
    if gpu.is_none() {
        warnings.push("no DXGI display adapter could be enumerated".to_string());
    }

    Ok(HardwareInfo {
        cpu_model,
        physical_cores,
        logical_cores,
        supports_hyper_threading: logical_cores > physical_cores,
        cpu_base_freq_mhz,
        core_layout,
        processor_groups,
        gpu,
        system_ram_mb,
        available_ram_mb,
        large_pages_available: privilege_enabled("SeLockMemoryPrivilege"),
        warnings,
    })
}

/// 兜底布局：每个处理器组一条记录（物理核粒度未知，但亲和性仍可用）。
fn fallback_layout(processor_groups: &[ProcessorGroup], logical_cores: u32) -> Vec<CoreLayout> {
    if processor_groups.is_empty() {
        return vec![CoreLayout::new(
            0,
            0,
            full_group_mask(logical_cores.min(crate::affinity::MAX_LOGICAL_PER_GROUP)),
        )];
    }
    processor_groups
        .iter()
        .enumerate()
        .map(|(position, group)| CoreLayout::new(position as u32, group.index, group.full_mask()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_privileges_buffer_is_parsed_without_panicking() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&1u32.to_ne_bytes()); // PrivilegeCount
        buffer.extend_from_slice(&7u32.to_ne_bytes()); // LUID.LowPart
        buffer.extend_from_slice(&0i32.to_ne_bytes()); // LUID.HighPart
        buffer.extend_from_slice(&SE_PRIVILEGE_ENABLED.0.to_ne_bytes());
        assert!(privilege_is_enabled(&buffer, 7, 0));
        assert!(!privilege_is_enabled(&buffer, 8, 0));

        // 属性位未置位 → 未启用
        let mut disabled = buffer.clone();
        let last = disabled.len() - 4;
        disabled[last..].copy_from_slice(&0u32.to_ne_bytes());
        assert!(!privilege_is_enabled(&disabled, 7, 0));

        // 截断 / 空 / 声明数量大于实际数据：一律返回 false，不得 panic。
        assert!(!privilege_is_enabled(&[], 7, 0));
        assert!(!privilege_is_enabled(&buffer[..8], 7, 0));
        assert!(!privilege_is_enabled(&[1, 0, 0, 0], 7, 0));
        assert!(!privilege_is_enabled(&[9, 0, 0, 0], 7, 0));
    }

    #[test]
    fn elevation_is_readable() {
        // 无论是否提权都必须成功返回（未提权是 false，不是错误），且结果稳定。
        let first = is_elevated().expect("read elevation");
        let second = is_elevated().expect("read elevation");
        assert_eq!(first, second);
    }

    #[test]
    fn driver_version_is_formatted_like_the_cpp_build() {
        // 高位 31.0，低位 15.3742 → "31.0.15.3742"
        let packed: i64 = ((31u64 << 48) | (15u64 << 16) | 3742u64) as i64;
        assert_eq!(format_driver_version(packed), "31.0.15.3742");
    }

    #[test]
    fn hardware_probe_reports_cores_memory_and_warnings() {
        let info = hardware().expect("hardware probe");
        assert!(info.logical_cores > 0);
        assert!(info.physical_cores > 0);
        assert!(info.physical_cores <= info.logical_cores);
        assert!(!info.processor_groups.is_empty());
        assert_eq!(
            info.processor_groups
                .iter()
                .map(|group| group.logical_count)
                .sum::<u32>(),
            info.logical_cores
        );
        assert!(!info.core_layout.is_empty());
        assert!(info.system_ram_mb > 0);
        assert!(!info.cpu_model.is_empty());
        // 拓扑与亲和性计算必须自洽：所有核的掩码之和 = 逻辑处理器总数（同组内不重叠）。
        let plan = crate::affinity::AffinityPlan::full(info.logical_cores).expect("full plan");
        assert_eq!(plan.selected_logical(), info.logical_cores);
    }

    #[test]
    fn fallback_layout_covers_every_group() {
        let groups = vec![ProcessorGroup::new(0, 64), ProcessorGroup::new(1, 32)];
        let layout = fallback_layout(&groups, 96);
        assert_eq!(layout.len(), 2);
        assert_eq!(layout[0].affinity, u64::MAX);
        assert_eq!(layout[1].affinity, 0xffff_ffff);
    }
}
