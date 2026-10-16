//! 单实例：命名互斥体 + 唤醒已有实例。
//!
//! 用 `Local\` 前缀而不是 `Global\`：每个登录会话一个实例才符合直觉
//! （多用户/远程桌面下互不打架）。

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, PostMessageW, RegisterWindowMessageW,
};

use crate::win::strings::to_wide;

/// 互斥体名（`Local\` = 当前会话）。
pub const MUTEX_NAME: &str = "Local\\ClipHist.SingleInstance";
/// 第二个实例用来唤起已有实例的自定义消息名。
pub const WAKE_MESSAGE: &str = "ClipHist.WakeUp";

/// 持有互斥体句柄；进程退出时由系统释放。
pub struct InstanceGuard {
    handle: HANDLE,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// 尝试成为唯一实例。
///
/// - `Some(guard)`：当前进程是第一个实例。
/// - `None`：已经有实例在跑（调用方应当 [`wake_existing`] 后立刻退出）。
///
/// 互斥体创建本身失败时不阻止启动：宁可多开一个，也不要让用户完全用不了。
pub fn acquire() -> Option<InstanceGuard> {
    let name = to_wide(MUTEX_NAME);
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return Some(InstanceGuard {
            handle: std::ptr::null_mut(),
        });
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(handle) };
        return None;
    }
    Some(InstanceGuard { handle })
}

/// 让已有实例把历史弹窗调出来。
pub fn wake_existing(window_class: &str) -> bool {
    let class = to_wide(window_class);
    let hwnd = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
    if hwnd.is_null() {
        return false;
    }
    let msg = unsafe { RegisterWindowMessageW(to_wide(WAKE_MESSAGE).as_ptr()) };
    if msg == 0 {
        return false;
    }
    unsafe { PostMessageW(hwnd, msg, 0, 0) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutex_is_exclusive_within_process() {
        // 同一个进程再取一次也必须拿到 ERROR_ALREADY_EXISTS。
        // 注意：这条测试会让"当前进程"一直持有互斥体到测试结束（Drop 释放）。
        let first = acquire();
        assert!(first.is_some(), "第一次获取必须成功");
        let second = acquire();
        assert!(second.is_none(), "第二次获取必须被判定为重复实例");
        drop(first);
        // 释放后应可再次获取
        let third = acquire();
        assert!(third.is_some(), "释放后应能重新获取");
    }
}
