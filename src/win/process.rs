//! 前台进程名查询：给每条历史记录打上"从哪个程序复制的"标签。
//!
//! 用 `PROCESS_QUERY_LIMITED_INFORMATION` 而不是 `PROCESS_QUERY_INFORMATION`：
//! 前者对更高完整性级别的进程也能拿到基本信息（不触发 UIPI 拒绝），
//! 拿不到就返回 `None`，绝不因此丢一条历史。

use windows_sys::Win32::Foundation::{CloseHandle, HWND};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::core::filter::file_name_of;

/// 当前前台窗口的进程可执行文件名（如 `chrome.exe`）。
pub fn foreground_process_name() -> Option<String> {
    process_name_of_window(unsafe { GetForegroundWindow() })
}

pub fn process_name_of_window(hwnd: HWND) -> Option<String> {
    if hwnd.is_null() {
        return None;
    }
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    process_name(pid)
}

pub fn process_name(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let name = file_name_of(&path);
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_name_is_resolvable() {
        // 当前测试进程必然可查；结果应当是本测试可执行文件名（.exe 结尾）。
        let name = process_name(std::process::id()).expect("自己的进程名必须能查到");
        assert!(name.to_lowercase().ends_with(".exe"), "实际得到 {name}");
    }

    #[test]
    fn invalid_pid_returns_none() {
        assert!(process_name(0xFFFF_FFF0).is_none());
    }

    #[test]
    fn null_window_returns_none() {
        assert!(process_name_of_window(std::ptr::null_mut()).is_none());
    }
}
