//! 剪贴板读写：`WM_CLIPBOARDUPDATE` 通知、多格式采集、多格式写回、模拟 Ctrl+V。
//!
//! 设计要点：
//! 1. **零轮询**：用 `AddClipboardFormatListener` 收系统通知，不用定时器，也不用
//!    `SetClipboardViewer` 链（后者会被别的程序打断）。
//! 2. **格式优先级**：`CF_HDROP` 先于图片格式 —— 资源管理器复制图片文件时可能会顺带
//!    提供 `CF_DIB` 缩略图，若先取图片就会丢掉"文件"语义，粘出来变成一张图而不是文件。
//! 3. **读的时候就把大小卡住**：`GlobalSize` 先看一眼，超上限直接不分配内存。
//! 4. **写回一定带多个格式**：文件同时给 `CF_HDROP` + `Preferred DropEffect` + 文本兜底，
//!    这样资源管理器能粘出文件，聊天软件也能粘出路径。

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_SHIFT,
    MOD_WIN, VK_CONTROL, VK_V,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
};

use crate::core::model::ImageData;
use crate::core::payload::{
    build_drop_effect, build_hdrop, join_paths_text, parse_drop_effect, parse_hdrop,
    DROPEFFECT_COPY, DROPEFFECT_MOVE,
};
use crate::image::dib::{dib_to_rgba, parse_dib};
use crate::image::thumb::decode_png_rgba;
use crate::win::strings::to_wide;

pub const CF_DIB: u32 = 8;
pub const CF_UNICODETEXT: u32 = 13;
pub const CF_HDROP: u32 = 15;
pub const CF_DIBV5: u32 = 17;

/// 要求剪贴板管理器"不要记录我"的格式名（密码管理器普遍使用）。
const EXCLUDE_FORMATS: [&str; 2] = [
    "Clipboard Viewer Ignore",
    "ExcludeClipboardContentFromMonitorProcessing",
];
/// 标记"复制/剪切"的格式名。
const PREFERRED_DROP_EFFECT: &str = "Preferred DropEffect";
/// 浏览器/绘图软件常见的原始 PNG 格式名。
const PNG_FORMAT: &str = "PNG";

/// 单次采集的数量与体积上限。
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_text_bytes: usize,
    pub max_image_pixels: i64,
    pub capture_images: bool,
    /// 一次最多接受多少个文件路径。
    pub max_files: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_text_bytes: 2 * 1024 * 1024,
            max_image_pixels: 12_000_000,
            capture_images: true,
            max_files: 1000,
        }
    }
}

/// 一次剪贴板读取的结果。
#[derive(Debug)]
pub enum Content {
    /// 没有我们认识的格式（例如只复制了一个文件流）。
    Empty,
    /// 对方明确要求不要记录。
    Ignored,
    Text(String),
    Files {
        paths: Vec<String>,
        is_cut: bool,
    },
    Image(ImageData),
    /// 认识格式但按策略跳过，附人类可读原因（用于气泡提示）。
    Skipped(&'static str),
}

// ------------------------------------------------------------------ 通知

pub fn add_listener(hwnd: HWND) -> bool {
    unsafe { AddClipboardFormatListener(hwnd) != 0 }
}

pub fn remove_listener(hwnd: HWND) -> bool {
    unsafe { RemoveClipboardFormatListener(hwnd) != 0 }
}

/// 剪贴板序号：每次内容变化 +1。用于识别"这次变化是不是我们自己写进去的"。
pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

pub fn format_id(name: &str) -> u32 {
    let wide = to_wide(name);
    unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
}

// ------------------------------------------------------------------ 读取

/// 打开剪贴板并读取内容。
///
/// `OpenClipboard` 会被别的程序短暂占用（浏览器、密码管理器尤其明显），
/// 所以带重试；重试仍失败时返回 [`Content::Empty`]，把这次变化丢掉而不是报错。
pub fn read(hwnd: HWND, limits: &Limits) -> Content {
    if !open_with_retry(hwnd, 5, 20) {
        return Content::Empty;
    }
    let content = read_locked(limits);
    unsafe { CloseClipboard() };
    content
}

fn open_with_retry(hwnd: HWND, attempts: u32, delay_ms: u64) -> bool {
    for i in 0..attempts {
        if unsafe { OpenClipboard(hwnd) } != 0 {
            return true;
        }
        if i + 1 < attempts {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
    }
    false
}

/// 调用前必须已经持有剪贴板。
fn read_locked(limits: &Limits) -> Content {
    // 1) 尊重"请勿记录"
    for name in EXCLUDE_FORMATS {
        let id = format_id(name);
        if id != 0 && unsafe { IsClipboardFormatAvailable(id) } != 0 {
            return Content::Ignored;
        }
    }

    // 2) 文件优先：资源管理器复制图片文件时也会给 CF_DIB，先取 DIB 会丢失文件语义。
    if unsafe { IsClipboardFormatAvailable(CF_HDROP) } != 0 {
        if let Some(bytes) = read_format(CF_HDROP, 64 * 1024 * 1024) {
            if let Some(paths) = parse_hdrop(&bytes) {
                if paths.is_empty() {
                    return Content::Empty;
                }
                if paths.len() > limits.max_files {
                    return Content::Skipped("复制的文件数量过多，未记录");
                }
                let is_cut = format_id(PREFERRED_DROP_EFFECT);
                let effect = if is_cut != 0 {
                    read_format(is_cut, 16)
                        .and_then(|b| parse_drop_effect(&b))
                        .unwrap_or(DROPEFFECT_COPY)
                } else {
                    DROPEFFECT_COPY
                };
                return Content::Files {
                    paths,
                    is_cut: effect & DROPEFFECT_MOVE != 0,
                };
            }
        }
        return Content::Empty;
    }

    // 3) 图片
    if limits.capture_images {
        let max_pixels = limits.max_image_pixels.max(1) as usize;
        let cap = max_pixels
            .saturating_mul(4)
            .saturating_add(4096)
            .min(256 * 1024 * 1024);

        let png_id = format_id(PNG_FORMAT);
        if png_id != 0 && unsafe { IsClipboardFormatAvailable(png_id) } != 0 {
            if let Some(bytes) = read_format(png_id, cap) {
                if let Ok(img) = decode_png_rgba(&bytes) {
                    if let Err(msg) = check_pixels(&img, limits) {
                        return Content::Skipped(msg);
                    }
                    return Content::Image(img);
                }
                // PNG 解不开就继续试 DIB（有些程序会挂一个损坏的 PNG 格式）。
            }
        }

        for fmt in [CF_DIBV5, CF_DIB] {
            if unsafe { IsClipboardFormatAvailable(fmt) } == 0 {
                continue;
            }
            let Some(bytes) = read_format(fmt, cap) else {
                return Content::Skipped("图片过大，未记录");
            };
            let Some(info) = parse_dib(&bytes) else {
                continue; // 不是我们支持的 DIB 形态，往下试文本
            };
            // 先按头部尺寸卡上限，再解码：避免为一张超限的图白白分配几十 MB。
            let pixels = info.width as i64 * info.height.unsigned_abs() as i64;
            if pixels > limits.max_image_pixels {
                return Content::Skipped("图片像素过多，未记录");
            }
            let Some((w, h, rgba)) = dib_to_rgba(&bytes, &info) else {
                return Content::Skipped("图片过大，未记录");
            };
            return Content::Image(ImageData {
                width: w,
                height: h,
                rgba,
            });
        }
    }

    // 4) 文本
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } != 0 {
        let size = clipboard_size(CF_UNICODETEXT);
        if size > limits.max_text_bytes + 2 {
            return Content::Skipped("文本内容过大，未记录");
        }
        if let Some(bytes) = read_format(CF_UNICODETEXT, limits.max_text_bytes + 2) {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let text = crate::win::strings::from_wide_until_nul(&units);
            if text.is_empty() {
                return Content::Empty;
            }
            return Content::Text(text);
        }
    }

    Content::Empty
}

/// 像素数上限校验。**只借用不克隆** —— 一张 4K 截图的像素缓冲是 33MB，
/// 校验时顺手 clone 一次就等于凭空多占 33MB。
fn check_pixels(img: &ImageData, limits: &Limits) -> Result<(), &'static str> {
    let pixels = img.width as i64 * img.height as i64;
    if pixels > limits.max_image_pixels {
        return Err("图片像素过多，未记录");
    }
    Ok(())
}

fn clipboard_size(fmt: u32) -> usize {
    let h = unsafe { GetClipboardData(fmt) };
    if h.is_null() {
        return 0;
    }
    unsafe { GlobalSize(h) }
}

/// 读取某个格式的全部字节。超过 `cap` 直接放弃（不分配）。
fn read_format(fmt: u32, cap: usize) -> Option<Vec<u8>> {
    let handle = unsafe { GetClipboardData(fmt) };
    if handle.is_null() {
        return None;
    }
    let size = unsafe { GlobalSize(handle) };
    if size == 0 || size > cap {
        return None;
    }
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        return None;
    }
    let out = unsafe { std::slice::from_raw_parts(ptr as *const u8, size) }.to_vec();
    unsafe { GlobalUnlock(handle) };
    Some(out)
}

// ------------------------------------------------------------------ 写回

/// 写回剪贴板的负载。
pub enum Payload<'a> {
    Text(&'a str),
    Files {
        paths: &'a [String],
        is_cut: bool,
    },
    /// `dibv5` 保证兼容性（画图/Word/PS），`png` 保证现代应用拿到无损原图。
    Image {
        dibv5: &'a [u8],
        png: &'a [u8],
    },
}

/// 一次打开会话写完所有格式。
///
/// `EmptyClipboard` 必须在我们持有剪贴板时调用，否则会把别人的数据清掉。
pub fn write(hwnd: HWND, payload: &Payload<'_>) -> Result<(), String> {
    if !open_with_retry(hwnd, 5, 20) {
        return Err("剪贴板被其他程序占用，请稍后重试".to_string());
    }
    let result = write_locked(payload);
    unsafe { CloseClipboard() };
    result
}

fn write_locked(payload: &Payload<'_>) -> Result<(), String> {
    if unsafe { EmptyClipboard() } == 0 {
        return Err("清空剪贴板失败".to_string());
    }
    match payload {
        Payload::Text(text) => {
            let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            set_bytes(CF_UNICODETEXT, crate::win::strings::wide_as_bytes(&utf16))
        }
        Payload::Files { paths, is_cut } => {
            set_bytes(CF_HDROP, &build_hdrop(paths))?;
            // 明确告诉目标"这是复制"，否则粘到文件夹里会变成移动（原文件消失）。
            let effect = if *is_cut {
                DROPEFFECT_MOVE
            } else {
                DROPEFFECT_COPY
            };
            let eff_id = format_id(PREFERRED_DROP_EFFECT);
            if eff_id != 0 {
                // 这个格式失败不影响文件粘贴，忽略错误。
                let _ = set_bytes(eff_id, &build_drop_effect(effect));
            }
            let text = join_paths_text(paths);
            let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = set_bytes(CF_UNICODETEXT, crate::win::strings::wide_as_bytes(&utf16));
            Ok(())
        }
        Payload::Image { dibv5, png } => {
            if dibv5.is_empty() {
                return Err("图片数据为空".to_string());
            }
            set_bytes(CF_DIBV5, dibv5)?;
            // CF_DIB 给只认老格式的程序兜底（头部为 V5 也普遍能读）。
            let _ = set_bytes(CF_DIB, dibv5);
            let png_id = format_id(PNG_FORMAT);
            if png_id != 0 && !png.is_empty() {
                let _ = set_bytes(png_id, png);
            }
            Ok(())
        }
    }
}

/// 写入任意注册格式（可选附带文本）。
///
/// 用途：自检时伪造"请勿记录"标记、以及将来做"忽略某个程序"时写自定义标记。
pub fn write_registered(name: &str, bytes: &[u8], text: Option<&str>) -> Result<(), String> {
    if !open_with_retry(std::ptr::null_mut(), 5, 20) {
        return Err("剪贴板被其他程序占用".to_string());
    }
    let result = (|| -> Result<(), String> {
        if unsafe { EmptyClipboard() } == 0 {
            return Err("清空剪贴板失败".to_string());
        }
        let id = format_id(name);
        if id == 0 {
            return Err(format!("注册剪贴板格式失败：{name}"));
        }
        set_bytes(id, bytes)?;
        if let Some(t) = text {
            let utf16: Vec<u16> = t.encode_utf16().chain(std::iter::once(0)).collect();
            set_bytes(CF_UNICODETEXT, crate::win::strings::wide_as_bytes(&utf16))?;
        }
        Ok(())
    })();
    unsafe { CloseClipboard() };
    result
}

/// 分配 `GMEM_MOVEABLE` 全局内存并写入剪贴板；成功后所有权归系统。
fn set_bytes(fmt: u32, bytes: &[u8]) -> Result<(), String> {
    let len = bytes.len().max(1);
    let handle: HANDLE = unsafe { GlobalAlloc(GMEM_MOVEABLE, len) };
    if handle.is_null() {
        return Err("剪贴板内存分配失败".to_string());
    }
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        unsafe { GlobalFree(handle) };
        return Err("剪贴板内存锁定失败".to_string());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        GlobalUnlock(handle);
    }
    let ret = unsafe { SetClipboardData(fmt, handle) };
    if ret.is_null() {
        // 失败时所有权仍在我们手上，必须自己释放，否则泄漏。
        unsafe { GlobalFree(handle) };
        return Err(format!("写入剪贴板格式 {fmt} 失败"));
    }
    Ok(())
}

// ------------------------------------------------------------------ 粘贴

pub fn foreground_window() -> HWND {
    unsafe { GetForegroundWindow() }
}

pub fn is_window(hwnd: HWND) -> bool {
    !hwnd.is_null() && unsafe { IsWindow(hwnd) } != 0
}

/// 把目标窗口切到前台并**校验结果**。
///
/// 只调 `SetForegroundWindow` 是不够的：Windows 的点击前置规则会静默拒绝，
/// 于是 Ctrl+V 会打进我们自己的弹窗。所以这里以 `GetForegroundWindow()` 的实际结果为准。
pub fn activate_window(hwnd: HWND) -> bool {
    if hwnd.is_null() || !is_window(hwnd) {
        return false;
    }
    unsafe { SetForegroundWindow(hwnd) };
    if unsafe { GetForegroundWindow() } == hwnd {
        return true;
    }
    // 兜底：把前台线程的输入队列临时挂到本线程，绕过"非前台进程不得抢焦点"的限制。
    unsafe {
        let cur = GetCurrentThreadId();
        let fg = GetForegroundWindow();
        let fg_tid = if fg.is_null() {
            0
        } else {
            GetWindowThreadProcessId(fg, std::ptr::null_mut())
        };
        if fg_tid != 0 && fg_tid != cur {
            AttachThreadInput(fg_tid, cur, 1);
            SetForegroundWindow(hwnd);
            AttachThreadInput(fg_tid, cur, 0);
        } else {
            SetForegroundWindow(hwnd);
        }
    }
    let now = unsafe { GetForegroundWindow() };
    now == hwnd
}

/// 模拟 Ctrl+V。返回是否四个事件都投递成功。
///
/// 目标窗口若以管理员权限运行，`SendInput` 会被 UIPI 静默丢弃 —— 调用方必须
/// 结合 [`activate_window`] 的返回值判断，并降级为"内容留在剪贴板 + 提示用户手按"。
pub fn send_ctrl_v() -> bool {
    let seq: [(u16, u32); 4] = [
        (VK_CONTROL, 0),
        (VK_V, 0),
        (VK_V, KEYEVENTF_KEYUP),
        (VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    let mut inputs = [INPUT::default(); 4];
    for (i, (vk, flags)) in seq.iter().enumerate() {
        inputs[i].r#type = INPUT_KEYBOARD;
        inputs[i].Anonymous.ki = KEYBDINPUT {
            wVk: *vk,
            wScan: 0,
            dwFlags: *flags,
            time: 0,
            dwExtraInfo: 0,
        };
    }
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    sent == inputs.len() as u32
}

/// 把设置里的修饰符换算成 Windows 的 `MOD_*`（两套常量数值本就一致，这里显式对齐一次）。
pub fn mods_to_win32(mods: u32) -> u32 {
    let mut out = 0;
    if mods & crate::core::settings::MOD_CONTROL != 0 {
        out |= MOD_CONTROL;
    }
    if mods & crate::core::settings::MOD_ALT != 0 {
        out |= MOD_ALT;
    }
    if mods & crate::core::settings::MOD_SHIFT != 0 {
        out |= MOD_SHIFT;
    }
    if mods & crate::core::settings::MOD_WIN != 0 {
        out |= MOD_WIN;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::settings;

    #[test]
    fn modifier_mapping_matches_settings_constants() {
        let all =
            settings::MOD_CONTROL | settings::MOD_ALT | settings::MOD_SHIFT | settings::MOD_WIN;
        assert_eq!(
            mods_to_win32(all),
            MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_WIN
        );
        assert_eq!(mods_to_win32(0), 0);
        assert_eq!(mods_to_win32(settings::MOD_CONTROL), MOD_CONTROL);
        // 设置层与 Win32 层的数值必须完全一致，否则热键会注册成别的组合。
        assert_eq!(settings::MOD_ALT, MOD_ALT);
        assert_eq!(settings::MOD_CONTROL, MOD_CONTROL);
        assert_eq!(settings::MOD_SHIFT, MOD_SHIFT);
        assert_eq!(settings::MOD_WIN, MOD_WIN);
    }

    #[test]
    fn format_constants_match_win32() {
        assert_eq!(CF_DIB, 8);
        assert_eq!(CF_UNICODETEXT, 13);
        assert_eq!(CF_HDROP, 15);
        assert_eq!(CF_DIBV5, 17);
    }

    #[test]
    fn null_window_is_not_a_window() {
        assert!(!is_window(std::ptr::null_mut()));
        assert!(!activate_window(std::ptr::null_mut()));
    }
}
