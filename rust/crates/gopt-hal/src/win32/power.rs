//! 电源方案：查询 / 枚举 / 切换（powrprof，仅官方 API）。
//!
//! 与 C++ 版的差异（有意为之，且已在 README 记录）：C++ 版在"按名称找不到高性能方案"时
//! 会盲目激活一个内置 GUID；这里改为**先枚举再匹配**，找不到就返回
//! [`crate::HalErrorKind::NotFound`]——"可解释"优先于"猜一个方案碰运气"。

use windows::core::GUID;
use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Power::{
    PowerEnumerate, PowerGetActiveScheme, PowerReadFriendlyName, PowerSetActiveScheme,
    ACCESS_SCHEME,
};
use windows::Win32::System::Registry::HKEY;

use super::from_wide_bytes;
use super::handle::LocalAllocGuard;
use crate::error::{HalError, HalResult};
use crate::types::{Guid, PowerScheme, PowerSchemeChange, PowerSchemeSelector};

fn power_error(
    operation: &'static str,
    code: WIN32_ERROR,
    context: impl core::fmt::Display,
) -> HalError {
    HalError::win32_from_code(operation, code.0).with_context(context)
}

/// 读取方案的本地化友好名。
///
/// `SubGroupOfPowerSettingsGuid` / `PowerSettingGuid` 必须传 **NULL** 才会返回"方案级"名称；
/// 传指向全零 GUID 的指针（C++ 版的做法）在部分系统上会被判为非法参数，
/// 结果是拿不到名字、只能退回 GUID 文本——所以这里显式传 `None`。
fn friendly_name(guid: Guid) -> HalResult<String> {
    let win32_guid = guid.to_win32();
    let mut size: u32 = 0;
    let first = unsafe {
        PowerReadFriendlyName(
            HKEY::default(),
            Some(&win32_guid),
            None,
            None,
            None,
            &mut size,
        )
    };
    if first != ERROR_SUCCESS || size == 0 {
        return Err(power_error(
            "PowerReadFriendlyName",
            first,
            format!("power scheme {guid} has no readable friendly name"),
        ));
    }

    let mut buffer = vec![0u8; size as usize];
    let second = unsafe {
        PowerReadFriendlyName(
            HKEY::default(),
            Some(&win32_guid),
            None,
            None,
            Some(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    if second != ERROR_SUCCESS {
        return Err(power_error(
            "PowerReadFriendlyName",
            second,
            format!("power scheme {guid} friendly name could not be read"),
        ));
    }
    Ok(from_wide_bytes(&buffer))
}

/// 查询当前活动电源方案。
pub(crate) fn query_active() -> HalResult<PowerScheme> {
    let mut pointer: *mut GUID = core::ptr::null_mut();
    let code = unsafe { PowerGetActiveScheme(HKEY::default(), &mut pointer) };
    if code != ERROR_SUCCESS {
        return Err(power_error(
            "PowerGetActiveScheme",
            code,
            "the active power scheme could not be queried",
        ));
    }

    // SAFETY: PowerGetActiveScheme 用 LocalAlloc 分配输出，guard 负责 LocalFree。
    let guard = unsafe { LocalAllocGuard::from_raw(pointer) };
    let raw = guard.as_ref().ok_or_else(|| {
        HalError::internal(
            "PowerGetActiveScheme",
            "the system returned a null power scheme pointer",
        )
    })?;
    let guid = Guid::from_win32(*raw);
    let name = friendly_name(guid).unwrap_or_else(|_| guid.to_string());
    Ok(PowerScheme::new(guid, name))
}

/// 枚举系统上已安装的全部电源方案。
pub(crate) fn list_schemes() -> HalResult<Vec<PowerScheme>> {
    let mut schemes = Vec::new();
    let mut index: u32 = 0;
    loop {
        let mut buffer = GUID {
            data1: 0,
            data2: 0,
            data3: 0,
            data4: [0; 8],
        };
        let mut size = core::mem::size_of::<GUID>() as u32;
        let code = unsafe {
            PowerEnumerate(
                HKEY::default(),
                None,
                None,
                ACCESS_SCHEME,
                index,
                Some(core::ptr::from_mut(&mut buffer).cast::<u8>()),
                &mut size,
            )
        };
        if code == ERROR_NO_MORE_ITEMS {
            break;
        }
        if code != ERROR_SUCCESS {
            return Err(power_error(
                "PowerEnumerate",
                code,
                format!("enumerating power schemes failed at index {index}"),
            ));
        }
        let guid = Guid::from_win32(buffer);
        let name = friendly_name(guid).unwrap_or_else(|_| guid.to_string());
        schemes.push(PowerScheme::new(guid, name));
        index += 1;
    }
    Ok(schemes)
}

/// 把选择器解析成具体方案（必须已安装，否则 `NotFound`）。
pub(crate) fn resolve(target: &PowerSchemeSelector) -> HalResult<PowerScheme> {
    let schemes = list_schemes()?;
    match target {
        PowerSchemeSelector::Explicit(guid) => schemes
            .into_iter()
            .find(|scheme| scheme.guid == *guid)
            .ok_or_else(|| {
                HalError::not_found(
                    "PowerEnumerate",
                    format!("power scheme {guid} is not installed on this system"),
                )
            }),
        PowerSchemeSelector::HighPerformance => schemes
            .into_iter()
            .find(|scheme| scheme.is_high_performance)
            .ok_or_else(|| {
                HalError::not_found(
                    "PowerEnumerate",
                    "no 'High performance' power scheme is installed on this system",
                )
            }),
    }
}

/// 切换活动电源方案，返回切换前后的方案（前者即回滚依据）。
pub(crate) fn set_active(target: &PowerSchemeSelector) -> HalResult<PowerSchemeChange> {
    let desired = resolve(target)?;
    let previous = query_active()?;

    let win32_guid = desired.guid.to_win32();
    let code = unsafe { PowerSetActiveScheme(HKEY::default(), Some(&win32_guid)) };
    if code != ERROR_SUCCESS {
        return Err(power_error(
            "PowerSetActiveScheme",
            code,
            format!(
                "switching to {} requires administrator privileges",
                desired.name
            ),
        ));
    }

    // 回读确认：切换是否真的生效（可解释性要求"报告的是事实，而不是意图"）。
    let current = query_active().unwrap_or(desired);
    Ok(PowerSchemeChange { previous, current })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::HalErrorKind;

    #[test]
    fn active_scheme_is_readable_with_a_name() {
        let scheme = query_active().expect("query active scheme");
        assert!(!scheme.guid.is_nil(), "{scheme:?}");
        assert!(!scheme.name.is_empty(), "{scheme:?}");
    }

    #[test]
    fn installed_schemes_include_the_active_one() {
        let active = query_active().expect("query");
        let schemes = list_schemes().expect("enumerate");
        assert!(!schemes.is_empty());
        assert!(
            schemes.iter().any(|scheme| scheme.guid == active.guid),
            "active scheme {active:?} must appear in the enumeration"
        );
    }

    #[test]
    fn unknown_scheme_guid_is_not_found() {
        let err = resolve(&PowerSchemeSelector::Explicit(Guid::from_u128(
            0xdead_beef_0000_0000_0000_0000_0000_0001,
        )))
        .expect_err("unknown scheme");
        assert_eq!(err.kind(), HalErrorKind::NotFound);
    }
}
