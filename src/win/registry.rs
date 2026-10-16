//! 注册表：只用于开机自启（`HKCU\...\Run`）。
//!
//! 刻意不引入 `winreg` crate：一个注册表值不值得多一个依赖，直接调 `advapi32` 更省。

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

use crate::win::strings::{from_wide_until_nul, to_wide, wide_as_bytes};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Run 键下的值名。固定不随版本变，否则升级后会留下两个自启项。
const VALUE_NAME: &str = "ClipHist";

/// 写入自启项。`command` 形如 `"C:\path\ClipHist.exe" --autostart`。
pub fn set_autostart(enabled: bool, command: &str) -> Result<(), String> {
    if !enabled {
        return disable_autostart();
    }

    let mut hkey: HKEY = std::ptr::null_mut();
    let sub = to_wide(RUN_KEY);
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            std::ptr::null(),
            0, // REG_OPTION_NON_VOLATILE
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut hkey,
            std::ptr::null_mut(),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(format!("打开自启注册表项失败（错误码 {rc}）"));
    }

    let name = to_wide(VALUE_NAME);
    let data = to_wide(command);
    let bytes = wide_as_bytes(&data);
    let rc = unsafe {
        RegSetValueExW(
            hkey,
            name.as_ptr(),
            0,
            REG_SZ,
            bytes.as_ptr(),
            bytes.len() as u32,
        )
    };
    unsafe { RegCloseKey(hkey) };
    if rc != ERROR_SUCCESS {
        return Err(format!("写入自启项失败（错误码 {rc}）"));
    }
    Ok(())
}

fn disable_autostart() -> Result<(), String> {
    let mut hkey: HKEY = std::ptr::null_mut();
    let sub = to_wide(RUN_KEY);
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
    };
    if rc != ERROR_SUCCESS {
        // 键不存在 = 本来就没开自启，不算错误。
        return Ok(());
    }
    let name = to_wide(VALUE_NAME);
    let rc = unsafe { RegDeleteValueW(hkey, name.as_ptr()) };
    unsafe { RegCloseKey(hkey) };
    if rc != ERROR_SUCCESS && rc != 2 {
        // 2 = ERROR_FILE_NOT_FOUND
        return Err(format!("删除自启项失败（错误码 {rc}）"));
    }
    Ok(())
}

/// 读取当前自启项内容；不存在返回 `None`。
pub fn autostart_value() -> Option<String> {
    let mut hkey: HKEY = std::ptr::null_mut();
    let sub = to_wide(RUN_KEY);
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut hkey,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let name = to_wide(VALUE_NAME);
    let mut buf = vec![0u16; 1024];
    let mut cb = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            name.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut u8,
            &mut cb,
        )
    };
    unsafe { RegCloseKey(hkey) };
    if rc != ERROR_SUCCESS {
        return None;
    }
    Some(from_wide_until_nul(&buf))
}

/// 自启是否已按我们期望的命令行开启（路径变了要能识别出来）。
pub fn is_autostart_enabled(expected_command: &str) -> bool {
    match autostart_value() {
        Some(v) => v.trim() == expected_command.trim(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_never_panics() {
        // 只验证接口是安全的：真实写注册表留给手工验收清单，不在单测里动用户系统。
        let _ = autostart_value();
        assert!(!is_autostart_enabled(""));
    }
}
