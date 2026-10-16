//! 托盘图标与弹出菜单。
//!
//! 图标不用 `.ico` 资源文件，而是运行时用 GDI 画进 32bpp DIB 再 `CreateIconIndirect` ——
//! 这样不需要 rc.exe / 资源脚本，构建链保持"一条 cargo build"。

use std::mem::zeroed;

use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateCompatibleDC, CreateDIBSection,
    CreateSolidBrush, DIB_RGB_COLORS, DeleteDC, DeleteObject, FillRect, GetDC, GetStockObject,
    HGDIOBJ, NULL_PEN, RGBQUAD, ReleaseDC, RoundRect, SelectObject,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_ERROR, NIIF_INFO,
    NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW,
    NOTIFYICON_VERSION_4, NOTIFY_ICON_INFOTIP_FLAGS, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    GetSystemMetrics, HICON, ICONINFO, IDI_APPLICATION, IMAGE_ICON, LR_SHARED, LoadImageW,
    SM_CXSMICON, SetForegroundWindow, TrackPopupMenu, WM_APP,
};

use crate::win::strings::copy_fixed;

/// 托盘回调消息号（`WM_APP + 1`，WM_APP 以上属于应用自定义区间）。
pub const TRAY_CALLBACK_MSG: u32 = WM_APP + 1;

// 菜单项标志（本地定义，避免依赖具体封装版本的常量命名）。
const MF_STRING: u32 = 0x0000;
const MF_SEPARATOR: u32 = 0x0800;
const MF_CHECKED: u32 = 0x0008;
const MF_GRAYED: u32 = 0x0001;
// TrackPopupMenu 标志。
const TPM_RIGHTBUTTON: u32 = 0x0002;
const TPM_NONOTIFY: u32 = 0x0080;
const TPM_RETURNCMD: u32 = 0x0100;

/// 托盘图标对象。`Drop` 负责摘掉图标，避免程序崩溃后留下"幽灵图标"。
pub struct Tray {
    hwnd: HWND,
    id: u32,
    icon: HICON,
    owns_icon: bool,
    tooltip: String,
    added: bool,
}

impl Tray {
    /// 创建并添加托盘图标。`icon` 为 `None` 时用系统默认应用程序图标。
    pub fn add(hwnd: HWND, id: u32, tooltip: &str) -> Option<Tray> {
        let (icon, owns_icon) = match create_app_icon() {
            Some(ic) => (ic, true),
            None => (load_stock_icon(), false),
        };
        let mut tray = Tray {
            hwnd,
            id,
            icon,
            owns_icon,
            tooltip: tooltip.to_string(),
            added: false,
        };

        let mut nid = tray.base(NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP);
        if unsafe { Shell_NotifyIconW(NIM_ADD, &nid) } == 0 {
            return None;
        }
        tray.added = true;

        // 声明使用 V4 行为：回调里 wParam/lParam 才是"坐标 + 事件"的编码方式。
        nid.uFlags = 0;
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &nid) };
        Some(tray)
    }

    fn base(&self, flags: u32) -> NOTIFYICONDATAW {
        // SAFETY: NOTIFYICONDATAW 是 POD，零初始化后再逐字段赋值是标准做法。
        let mut nid: NOTIFYICONDATAW = unsafe { zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = self.id;
        nid.uFlags = flags;
        nid.uCallbackMessage = TRAY_CALLBACK_MSG;
        nid.hIcon = self.icon;
        copy_fixed(&mut nid.szTip, &self.tooltip);
        nid
    }

    pub fn set_tooltip(&mut self, tooltip: &str) {
        self.tooltip = tooltip.to_string();
        if !self.added {
            return;
        }
        let nid = self.base(NIF_TIP | NIF_SHOWTIP);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    /// 气泡提示。用于"内容过大已跳过""热键被占用""自动粘贴失败"这类降级通知。
    pub fn balloon(&self, title: &str, text: &str, warning: bool) {
        if !self.added {
            return;
        }
        let mut nid = self.base(NIF_INFO);
        copy_fixed(&mut nid.szInfoTitle, title);
        copy_fixed(&mut nid.szInfo, text);
        nid.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
        // V4 下超时由系统控制，这里仍给一个值以兼容旧行为。
        nid.Anonymous.uTimeout = 5000;
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    pub fn remove(&mut self) {
        if !self.added {
            return;
        }
        let nid = self.base(0);
        unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
        self.added = false;
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        if self.owns_icon && !self.icon.is_null() {
            unsafe { DestroyIcon(self.icon) };
        }
    }
}

/// 托盘/气泡图标标志（对外暴露，避免 app 层依赖 windows-sys 的常量名）。
pub fn info_flag() -> NOTIFY_ICON_INFOTIP_FLAGS {
    NIIF_INFO
}
pub fn warning_flag() -> NOTIFY_ICON_INFOTIP_FLAGS {
    NIIF_WARNING
}
pub fn error_flag() -> NOTIFY_ICON_INFOTIP_FLAGS {
    NIIF_ERROR
}

/// 托盘回调的载荷解码（NOTIFYICON_VERSION_4 约定）：
/// `lParam` 低 16 位是事件，`wParam` 低/高 16 位是光标坐标（有符号）。
pub fn decode_callback(wparam: usize, lparam: isize) -> (u32, i32, i32) {
    let event = (lparam as u32) & 0xFFFF;
    let x = (wparam as u32 & 0xFFFF) as u16 as i16 as i32;
    let y = ((wparam as u32 >> 16) & 0xFFFF) as u16 as i16 as i32;
    (event, x, y)
}

/// 菜单项：`id == 0` 且 `label` 为空表示分隔线。
pub struct MenuItem<'a> {
    pub id: u32,
    pub label: &'a str,
    pub checked: bool,
    pub enabled: bool,
}

impl<'a> MenuItem<'a> {
    pub fn new(id: u32, label: &'a str) -> Self {
        Self {
            id,
            label,
            checked: false,
            enabled: true,
        }
    }
    pub fn checked(mut self, yes: bool) -> Self {
        self.checked = yes;
        self
    }
    pub fn enabled(mut self, yes: bool) -> Self {
        self.enabled = yes;
        self
    }
    pub fn separator() -> Self {
        Self {
            id: 0,
            label: "",
            checked: false,
            enabled: false,
        }
    }
}

/// 在指定位置弹出菜单，返回被选中的 id（未选择返回 `None`）。
///
/// 弹出前必须 `SetForegroundWindow`：否则菜单不会因为点击别处而消失（Win32 的经典坑）。
pub fn popup_menu(hwnd: HWND, x: i32, y: i32, items: &[MenuItem<'_>]) -> Option<u32> {
    let hmenu = unsafe { CreatePopupMenu() };
    if hmenu.is_null() {
        return None;
    }
    for it in items {
        if it.label.is_empty() {
            unsafe { AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null()) };
            continue;
        }
        let mut flags = MF_STRING;
        if it.checked {
            flags |= MF_CHECKED;
        }
        if !it.enabled {
            flags |= MF_GRAYED;
        }
        let wide = crate::win::strings::to_wide(it.label);
        unsafe { AppendMenuW(hmenu, flags, it.id as usize, wide.as_ptr()) };
    }

    unsafe { SetForegroundWindow(hwnd) };
    let cmd = unsafe {
        TrackPopupMenu(
            hmenu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            x,
            y,
            0,
            hwnd,
            std::ptr::null(),
        )
    };
    unsafe { DestroyMenu(hmenu) };
    if cmd == 0 {
        None
    } else {
        Some(cmd as u32)
    }
}

/// 光标位置。
pub fn cursor_pos() -> POINT {
    let mut p = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut p) };
    p
}

// ------------------------------------------------------------------ 图标

/// 载入系统默认应用程序图标（`CreateIconIndirect` 失败时的兜底）。
pub fn load_stock_icon() -> HICON {
    let size = small_icon_size();
    unsafe {
        LoadImageW(
            std::ptr::null_mut(),
            IDI_APPLICATION,
            IMAGE_ICON,
            size,
            size,
            LR_SHARED,
        ) as HICON
    }
}

fn small_icon_size() -> i32 {
    let s = unsafe { GetSystemMetrics(SM_CXSMICON) };
    if s <= 0 { 16 } else { s }
}

/// 运行时绘制托盘图标：一张浅色"卡片 + 夹子 + 两行文字"，在高对比主题下也看得清。
///
/// 实现要点（GDI 的经典陷阱）：**GDI 画像素时不会写 alpha 通道**，所以先在整块画布上
/// 填一个哨兵色，画完再按"RGB 是否等于哨兵色"判定透明度；用 GDI 输出的 alpha 会全 0，
/// 结果就是一张全透明、什么都看不见的图标。
pub fn create_app_icon() -> Option<HICON> {
    let size = small_icon_size();
    let size = size.clamp(16, 64);

    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return None;
        }
        let mem = CreateCompatibleDC(screen);
        if mem.is_null() {
            ReleaseDC(std::ptr::null_mut(), screen);
            return None;
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // 负数 = 自上而下，方便按行索引
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color_bmp = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if color_bmp.is_null() || bits.is_null() {
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
            return None;
        }
        let old_bmp = SelectObject(mem, color_bmp as HGDIOBJ);
        let old_pen = SelectObject(mem, GetStockObject(NULL_PEN));

        // 哨兵色：一种不可能出现在图标里的洋红。
        const SENTINEL: u32 = 0x00FF_00FF; // COLORREF: 0x00BBGGRR
        let sentinel_brush = CreateSolidBrush(SENTINEL);
        let full = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        FillRect(mem, &full, sentinel_brush);
        DeleteObject(sentinel_brush as HGDIOBJ);

        let s = size as f32;
        let px = |v: f32| (v * s).round() as i32;

        // 卡片本体
        let card = CreateSolidBrush(rgb(232, 236, 242));
        let old_brush = SelectObject(mem, card as HGDIOBJ);
        RoundRect(mem, px(0.08), px(0.16), px(0.92), px(0.94), px(0.18), px(0.18));
        SelectObject(mem, old_brush);
        DeleteObject(card as HGDIOBJ);

        // 顶部夹子
        let clip = CreateSolidBrush(rgb(96, 106, 122));
        let old_brush = SelectObject(mem, clip as HGDIOBJ);
        let r = RECT {
            left: px(0.34),
            top: px(0.04),
            right: px(0.66),
            bottom: px(0.30),
        };
        FillRect(mem, &r, clip);
        SelectObject(mem, old_brush);
        DeleteObject(clip as HGDIOBJ);

        // 两行"文字"
        let line = CreateSolidBrush(rgb(122, 132, 148));
        let old_brush = SelectObject(mem, line as HGDIOBJ);
        for (x0, x1, y0, y1) in [(0.24f32, 0.76f32, 0.42f32, 0.54f32), (0.24, 0.66, 0.64, 0.76)] {
            let r = RECT {
                left: px(x0),
                top: px(y0),
                right: px(x1),
                bottom: px(y1),
            };
            FillRect(mem, &r, line);
        }
        SelectObject(mem, old_brush);
        DeleteObject(line as HGDIOBJ);

        // 后处理：哨兵色 → 透明；其余 → 不透明。
        let pixels = bits as *mut u8;
        let count = (size * size) as usize;
        for i in 0..count {
            let p = pixels.add(i * 4);
            let b = *p as u32;
            let g = *p.add(1) as u32;
            let r = *p.add(2) as u32;
            let rgb = b | (g << 8) | (r << 16);
            if rgb == SENTINEL {
                for k in 0..4 {
                    *p.add(k) = 0;
                }
            } else {
                *p.add(3) = 255;
            }
        }

        SelectObject(mem, old_pen);
        SelectObject(mem, old_bmp);

        // AND 掩码：全 0（形状完全交给 alpha 通道）。CreateBitmap 不保证清零，
        // 所以显式给一块全 0 缓冲（1bpp，行按 WORD 对齐，自下而上）。
        let stride = ((size as usize + 15) / 16) * 2;
        let mask_bits = vec![0u8; stride * size as usize];
        let mask_bmp = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr() as *const core::ffi::c_void);

        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask_bmp,
            hbmColor: color_bmp,
        };
        let icon = CreateIconIndirect(&info);

        DeleteObject(mask_bmp as HGDIOBJ);
        DeleteObject(color_bmp as HGDIOBJ);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), screen);

        if icon.is_null() { None } else { Some(icon) }
    }
}

#[inline]
fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_decoding_matches_v4_encoding() {
        // wParam = (y << 16) | x，lParam 低 16 位 = 事件
        let wparam = ((300u32) << 16) | 100u32;
        let (event, x, y) = decode_callback(wparam as usize, 0x0205); // WM_RBUTTONUP
        assert_eq!(event, 0x0205);
        assert_eq!(x, 100);
        assert_eq!(y, 300);
    }

    #[test]
    fn callback_supports_negative_virtual_screen_coords() {
        // 副屏在主屏左侧时坐标是负的：必须按有符号解释。
        let wparam = ((-1920i32 as u32 & 0xFFFF) << 16) | (-50i32 as u32 & 0xFFFF);
        let (_, x, y) = decode_callback(wparam as usize, 0x0202);
        assert_eq!(x, -50);
        assert_eq!(y, -1920);
    }

    #[test]
    fn menu_item_builders() {
        let it = MenuItem::new(7, "显示历史");
        assert_eq!(it.id, 7);
        assert!(it.enabled);
        assert!(!it.checked);
        assert!(MenuItem::separator().label.is_empty());
        let it = MenuItem::new(1, "暂停记录").checked(true).enabled(false);
        assert!(it.checked && !it.enabled);
    }

    #[test]
    fn colorref_layout_is_bgr() {
        assert_eq!(rgb(255, 0, 0), 0x0000_00FF, "红色在低位");
        assert_eq!(rgb(0, 0, 255), 0x00FF_0000, "蓝色在高位");
    }
}
