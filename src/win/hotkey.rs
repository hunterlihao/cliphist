//! 全局热键注册。
//!
//! 注册失败（组合键被别的程序占用）不是致命错误：调用方会退回托盘菜单并提示用户改键。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT,
};

/// 主热键 id。
pub const HOTKEY_SHOW: i32 = 1;
/// 探测用 id：设置窗口拿它试注册一次，用来在改键当下就告诉用户"这个组合键能不能用"。
pub const HOTKEY_PROBE: i32 = 2;

/// 注册全局热键。
///
/// 强制带上 `MOD_NOREPEAT`：否则按住不放会连续收到 `WM_HOTKEY`，弹窗被反复开关。
pub fn register(hwnd: HWND, id: i32, mods: u32, vk: u32) -> bool {
    unsafe { RegisterHotKey(hwnd, id, mods | MOD_NOREPEAT, vk) != 0 }
}

pub fn unregister(hwnd: HWND, id: i32) {
    unsafe { UnregisterHotKey(hwnd, id) };
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::MOD_NOREPEAT as M;

    #[test]
    fn norepeat_flag_matches_win32_documentation() {
        // 0x4000 —— 写死这个值是为了让"按下不放不重复触发"这件事有据可查。
        assert_eq!(M, 0x4000);
    }
}
