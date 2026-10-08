//! RAII 句柄封装：所有内核/注册表/LocalAlloc 资源都由 newtype 持有并在 `Drop` 中释放。
//!
//! 目的：把 Win32 里最常见的一类缺陷（提前 return 漏掉 `CloseHandle`/`RegCloseKey`）变成
//! 编译器保证的性质。Win32 后端里不允许出现裸 `CloseHandle(...)` 调用，只允许构造
//! [`OwnedHandle`] / [`OwnedRegKey`]。
//!
//! 特别注意伪句柄：`GetCurrentProcess()` 返回 `(HANDLE)-1`，对它调用 `CloseHandle` 是
//! 未定义行为。因此进程句柄用 [`ProcessHandle`] 区分"伪句柄"与"真实句柄"，
//! 从类型上排除这个错误。

use core::marker::PhantomData;

use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows::Win32::System::Registry::{RegCloseKey, HKEY};
use windows::Win32::System::Threading::GetCurrentProcess;

/// 拥有所有权的内核句柄（进程/线程/快照）。
#[derive(Debug)]
pub(crate) struct OwnedHandle {
    raw: HANDLE,
}

impl OwnedHandle {
    /// 接管一个需要关闭的句柄的所有权。
    pub(crate) fn new(raw: HANDLE) -> Self {
        Self { raw }
    }

    /// 原始句柄值（借用，不转移所有权）。
    pub(crate) fn raw(&self) -> HANDLE {
        self.raw
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.raw.0.is_null() {
            // CloseHandle 的返回值无法补救；此处显式忽略以符合"不用 panic 表达失败"。
            unsafe {
                let _ = CloseHandle(self.raw);
            }
        }
    }
}

/// 进程句柄：区分伪句柄与真实句柄，避免误关 `(HANDLE)-1`。
#[derive(Debug)]
pub(crate) enum ProcessHandle {
    /// `GetCurrentProcess()` 的伪句柄：永远有效、永远不能关闭。
    PseudoCurrent,
    /// 由 `OpenProcess` 取得的真实句柄：`Drop` 时自动关闭。
    Owned(OwnedHandle),
}

impl ProcessHandle {
    /// 接管 `OpenProcess` 返回的句柄。
    pub(crate) fn owned(raw: HANDLE) -> Self {
        ProcessHandle::Owned(OwnedHandle::new(raw))
    }

    /// 目标句柄的原始值。
    pub(crate) fn raw(&self) -> HANDLE {
        match self {
            ProcessHandle::PseudoCurrent => unsafe { GetCurrentProcess() },
            ProcessHandle::Owned(handle) => handle.raw(),
        }
    }
}

/// 拥有所有权的注册表键（`Drop` 时 `RegCloseKey`）。
#[derive(Debug)]
pub(crate) struct OwnedRegKey {
    raw: HKEY,
}

impl OwnedRegKey {
    /// 接管一个已打开的注册表键。
    pub(crate) fn new(raw: HKEY) -> Self {
        Self { raw }
    }

    /// 原始键句柄。
    pub(crate) fn raw(&self) -> HKEY {
        self.raw
    }
}

impl Drop for OwnedRegKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.raw);
        }
    }
}

/// `PowerGetActiveScheme` 之类"由系统 LocalAlloc 分配、需 LocalFree 释放"的缓冲区。
#[derive(Debug)]
pub(crate) struct LocalAllocGuard<T> {
    ptr: *mut T,
    _marker: PhantomData<T>,
}

impl<T> LocalAllocGuard<T> {
    /// # Safety
    ///
    /// `ptr` 必须是由 `LocalAlloc`（或系统以 LocalAlloc 分配的 API 输出）返回的指针，
    /// 且所有权未被其它代码接管；传入栈地址会破坏内存安全。
    pub(crate) unsafe fn from_raw(ptr: *mut T) -> Self {
        Self {
            ptr,
            _marker: PhantomData,
        }
    }

    /// 只读访问缓冲区内容。
    pub(crate) fn as_ref(&self) -> Option<&T> {
        if self.ptr.is_null() {
            None
        } else {
            // SAFETY: 调用方保证 ptr 指向至少一个有效的 T 且在本 guard 生命周期内有效。
            Some(unsafe { &*self.ptr })
        }
    }
}

impl<T> Drop for LocalAllocGuard<T> {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                let _ = LocalFree(HLOCAL(self.ptr.cast()));
            }
            self.ptr = core::ptr::null_mut();
        }
    }
}
