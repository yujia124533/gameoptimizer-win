//! 开机启动项：HKCU/HKLM 的 `...\CurrentVersion\Run` 键（advapi32 注册表 API）。
//!
//! 禁用策略与 C++ 版逐字节一致（备份文件可互操作）：**改名迁移**
//! `Foo` → `[disabled] Foo`，值内容与类型原样保留，因此禁用/启用天然可逆、可回滚。
//!
//! 安全细节：
//!
//! * 只碰 Run 键，不碰 `RunOnce` / 计划任务 / 服务；
//! * 目标名已存在时**拒绝覆盖**并报错（避免"禁用 A 顺手毁掉 B"）；
//! * 新值写入后删旧值失败时，尽力删掉刚写的新值，保持"要么完全成功、要么状态不变"；
//! * HKLM 写入需要管理员，未提权时返回 [`crate::HalErrorKind::AccessDenied`]（含原始错误码 5）。

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS,
};
use windows::Win32::System::Registry::{
    RegDeleteValueW, RegEnumValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY,
    KEY_WRITE, REG_EXPAND_SZ, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
};

use super::handle::OwnedRegKey;
use super::{from_wide, to_wide};
use crate::error::{HalError, HalErrorKind, HalResult};
use crate::types::{RunEntry, RunHive};

/// 两个根共用的 Run 子键路径。
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn root_key(hive: RunHive) -> HKEY {
    match hive {
        RunHive::CurrentUser => HKEY_CURRENT_USER,
        RunHive::LocalMachine => HKEY_LOCAL_MACHINE,
    }
}

fn open_run_key(hive: RunHive, access: REG_SAM_FLAGS) -> HalResult<OwnedRegKey> {
    let path = to_wide(RUN_KEY_PATH);
    let mut key = HKEY::default();
    let code = unsafe { RegOpenKeyExW(root_key(hive), PCWSTR(path.as_ptr()), 0, access, &mut key) };
    if code != ERROR_SUCCESS {
        let error = HalError::win32_from_code("RegOpenKeyExW", code.0)
            .with_context(format!("{}\\{RUN_KEY_PATH}", hive.as_str()));
        return Err(if code == ERROR_ACCESS_DENIED {
            error.with_context(format!(
                "{} requires administrator privileges",
                hive.as_str()
            ))
        } else {
            error
        });
    }
    Ok(OwnedRegKey::new(key))
}

/// 枚举键下的值名（不读取数据，避免为了拿名字而分配大缓冲）。
fn enum_value_names(key: &OwnedRegKey) -> HalResult<Vec<String>> {
    let mut max_name_len: u32 = 0;
    let mut value_count: u32 = 0;
    unsafe {
        // 失败不致命：容量退化为默认值，后续 ERROR_MORE_DATA 会自动扩容。
        let _ = RegQueryInfoKeyW(
            key.raw(),
            PWSTR::null(),
            None,
            None,
            None,
            None,
            None,
            Some(&mut value_count),
            Some(&mut max_name_len),
            None,
            None,
            None,
        );
    }

    let mut capacity = (max_name_len as usize).max(64) + 2;
    let mut names = Vec::with_capacity(value_count as usize);
    let mut index: u32 = 0;
    loop {
        let mut buffer = vec![0u16; capacity];
        let mut length = capacity as u32;
        let code = unsafe {
            RegEnumValueW(
                key.raw(),
                index,
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if code == ERROR_NO_MORE_ITEMS {
            break;
        }
        if code == ERROR_MORE_DATA {
            capacity = (length as usize).max(capacity) + 2;
            continue; // 同一 index 重试
        }
        if code != ERROR_SUCCESS {
            return Err(HalError::win32_from_code("RegEnumValueW", code.0)
                .with_context(format!("value index {index}")));
        }
        names.push(from_wide(&buffer[..length as usize]));
        index += 1;
    }
    Ok(names)
}

/// 读取值的数据与类型。
fn read_value(key: &OwnedRegKey, name: &str) -> HalResult<(Vec<u8>, REG_VALUE_TYPE)> {
    let wide = to_wide(name);
    let mut kind = REG_VALUE_TYPE(0);
    let mut size: u32 = 0;
    let probe = unsafe {
        RegQueryValueExW(
            key.raw(),
            PCWSTR(wide.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    if probe == ERROR_FILE_NOT_FOUND {
        return Err(HalError::not_found(
            "RegQueryValueExW",
            format!("startup entry `{name}` does not exist"),
        ));
    }
    if probe != ERROR_SUCCESS {
        return Err(HalError::win32_from_code("RegQueryValueExW", probe.0)
            .with_context(format!("value `{name}`")));
    }

    let mut buffer = vec![0u8; size.max(1) as usize];
    let mut read_size = size;
    let code = unsafe {
        RegQueryValueExW(
            key.raw(),
            PCWSTR(wide.as_ptr()),
            None,
            Some(&mut kind),
            Some(buffer.as_mut_ptr()),
            Some(&mut read_size),
        )
    };
    if code != ERROR_SUCCESS {
        return Err(HalError::win32_from_code("RegQueryValueExW", code.0)
            .with_context(format!("value `{name}`")));
    }
    buffer.truncate(read_size as usize);
    Ok((buffer, kind))
}

/// 以原类型写回值（保持 `REG_SZ` / `REG_EXPAND_SZ` 语义不变）。
fn write_value(key: &OwnedRegKey, name: &str, bytes: &[u8], kind: REG_VALUE_TYPE) -> HalResult<()> {
    let wide = to_wide(name);
    let code = unsafe { RegSetValueExW(key.raw(), PCWSTR(wide.as_ptr()), 0, kind, Some(bytes)) };
    if code != ERROR_SUCCESS {
        let error = HalError::win32_from_code("RegSetValueExW", code.0)
            .with_context(format!("writing startup entry `{name}`"));
        return Err(if code == ERROR_ACCESS_DENIED {
            error.with_context("writing HKLM startup entries requires administrator privileges")
        } else {
            error
        });
    }
    Ok(())
}

fn delete_value(key: &OwnedRegKey, name: &str) -> HalResult<()> {
    let wide = to_wide(name);
    let code = unsafe { RegDeleteValueW(key.raw(), PCWSTR(wide.as_ptr())) };
    if code != ERROR_SUCCESS {
        return Err(HalError::win32_from_code("RegDeleteValueW", code.0)
            .with_context(format!("removing startup entry `{name}`")));
    }
    Ok(())
}

/// Run 值 → 文本；非字符串类型返回 `None`（Windows 自身也会忽略这类值）。
fn decode_string_value(bytes: &[u8], kind: REG_VALUE_TYPE) -> Option<String> {
    if kind != REG_SZ && kind != REG_EXPAND_SZ {
        return None;
    }
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    Some(from_wide(&units))
}

/// 列出全部启动项（含已禁用项；按根 + 展示名排序）。
pub(crate) fn list_run_entries() -> HalResult<Vec<RunEntry>> {
    let mut entries = Vec::new();
    for hive in RunHive::ALL {
        let key = open_run_key(hive, KEY_READ | KEY_WOW64_64KEY)?;
        for name in enum_value_names(&key)? {
            let (bytes, kind) = read_value(&key, &name)?;
            let Some(command) = decode_string_value(&bytes, kind) else {
                continue; // 非字符串值不是有效启动项
            };
            entries.push(RunEntry::from_registry(
                hive,
                name,
                command,
                kind == REG_EXPAND_SZ,
            ));
        }
    }
    entries.sort_by(|a, b| (a.hive, &a.name).cmp(&(b.hive, &b.name)));
    Ok(entries)
}

/// 启用/禁用启动项（按展示名定位，幂等）。
pub(crate) fn set_run_entry_enabled(
    hive: RunHive,
    name: &str,
    enabled: bool,
) -> HalResult<RunEntry> {
    let display = RunEntry::display_name(name).to_string();
    if display.trim().is_empty() {
        return Err(HalError::invalid_argument(
            "set_run_entry_enabled",
            "the startup entry name must not be empty",
        ));
    }
    let key = open_run_key(hive, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)?;

    let source = if enabled {
        RunEntry::disabled_value_name(&display)
    } else {
        display.clone()
    };
    let target = if enabled {
        display.clone()
    } else {
        RunEntry::disabled_value_name(&display)
    };

    let (bytes, kind) = match read_value(&key, &source) {
        Ok(value) => value,
        Err(error) if error.kind() == HalErrorKind::NotFound => {
            // 源不存在：若目标已经是期望状态，则视为幂等成功；否则确实没有这个启动项。
            let (bytes, kind) = read_value(&key, &target)?;
            let command = decode_string_value(&bytes, kind).unwrap_or_default();
            return Ok(RunEntry::from_registry(
                hive,
                target,
                command,
                kind == REG_EXPAND_SZ,
            ));
        }
        Err(error) => return Err(error),
    };

    if read_value(&key, &target).is_ok() {
        return Err(HalError::invalid_argument(
            "RegSetValueExW",
            format!("startup entry `{target}` already exists; refusing to overwrite it"),
        ));
    }

    write_value(&key, &target, &bytes, kind)?;
    if let Err(error) = delete_value(&key, &source) {
        // 尽力回滚：删掉刚写入的新名字，保持"要么完全成功、要么状态不变"。
        let _ = delete_value(&key, &target);
        return Err(error.with_context("startup entry rename was rolled back"));
    }

    let command = decode_string_value(&bytes, kind).unwrap_or_default();
    Ok(RunEntry::from_registry(
        hive,
        target,
        command,
        kind == REG_EXPAND_SZ,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_key_path_matches_the_cpp_implementation() {
        assert_eq!(RUN_KEY_PATH, RunHive::CurrentUser.key_path());
        assert_eq!(RUN_KEY_PATH, RunHive::LocalMachine.key_path());
    }

    #[test]
    fn run_entries_are_readable_without_elevation() {
        let entries = list_run_entries().expect("list startup entries");
        // 断言的是"结构正确"，而不是"系统上一定有启动项"（CI 上可能为空）。
        for entry in &entries {
            assert!(!entry.name.is_empty(), "{entry:?}");
            assert_eq!(entry.enabled, !RunEntry::is_disabled(&entry.value_name));
            assert_eq!(entry.name, RunEntry::display_name(&entry.value_name));
        }
        let sorted: Vec<String> = entries.iter().map(RunEntry::id).collect();
        let mut expected = sorted.clone();
        expected.sort();
        assert_eq!(sorted, expected, "entries must be sorted by hive + name");
    }

    #[test]
    fn missing_entry_is_not_found_and_changes_nothing() {
        // 只读安全性：这个名字刻意不可能存在，因此本测试不会改动系统状态。
        let err = set_run_entry_enabled(
            RunHive::CurrentUser,
            "gopt-hal-definitely-not-a-real-entry",
            false,
        )
        .expect_err("no such entry");
        assert_eq!(err.kind(), HalErrorKind::NotFound);
    }

    #[test]
    fn empty_name_is_rejected_before_touching_the_registry() {
        let err =
            set_run_entry_enabled(RunHive::CurrentUser, "   ", false).expect_err("empty name");
        assert_eq!(err.kind(), HalErrorKind::InvalidArgument);
    }

    #[test]
    fn string_value_decoding_handles_types_and_alignment() {
        let utf16: Vec<u8> = "C:\\game.exe"
            .encode_utf16()
            .chain(core::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(
            decode_string_value(&utf16, REG_SZ).as_deref(),
            Some("C:\\game.exe")
        );
        assert!(decode_string_value(&utf16, REG_EXPAND_SZ).is_some());
        // 非字符串类型（例如 REG_DWORD）不参与启动项管理。
        assert_eq!(decode_string_value(&[1, 0, 0, 0], REG_VALUE_TYPE(4)), None);
        // 奇数长度 / 空数据不允许 panic。
        assert_eq!(decode_string_value(&[0x41], REG_SZ).as_deref(), Some(""));
        assert_eq!(decode_string_value(&[], REG_SZ).as_deref(), Some(""));
    }
}
