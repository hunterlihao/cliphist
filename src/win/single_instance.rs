//! 单实例：命名互斥体 + 唤醒已有实例。
//!
//! 用 `Local\` 前缀而不是 `Global\`：每个登录会话一个实例才符合直觉
//! （多用户/远程桌面下互不打架）。

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
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
pub fn acquire() -> Option<InstanceGuard> {
    acquire_named(MUTEX_NAME)
}

/// 用指定名字获取互斥体。
///
/// 单独暴露带名字的版本，是为了让测试用**自己的**名字：否则单测会去抢真实应用实例
/// 正在持有的 `Local\ClipHist.SingleInstance`，变成"程序开着的时候 cargo test 就失败"
/// 这种环境依赖 —— 测试不该因为用户正在用软件而变红。
///
/// 互斥体创建本身失败时不阻止启动：宁可多开一个，也不要让用户完全用不了。
pub fn acquire_named(name: &str) -> Option<InstanceGuard> {
    let name = to_wide(name);
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
        // 用独立名字，避免与正在运行的真实实例抢同一个互斥体。
        let name = format!("Local\\ClipHist.Test.{}", std::process::id());
        let first = acquire_named(&name);
        assert!(first.is_some(), "第一次获取必须成功");
        let second = acquire_named(&name);
        assert!(second.is_none(), "第二次获取必须被判定为重复实例");
        drop(second);
        drop(first);
        // 释放后应可再次获取
        let third = acquire_named(&name);
        assert!(third.is_some(), "释放后应能重新获取");
    }
}
