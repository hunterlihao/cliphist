//! 设置窗口：自绘行式面板。
//!
//! 刻意不用 BUTTON / COMBOBOX 等控件：不给 comctl32 v6 清单文件就得忍受 Win95 外观的
//! 3D 边框，还不如自己画一排"标签 + 值"。整窗只有一个交互模型 —— 点行/点值的左右半边，
//! 因此也没有焦点管理、没有 Tab 顺序、没有消息泵，窗口过程同样不会被重入。

use std::mem::zeroed;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, InvalidateRect, PAINTSTRUCT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
    GetClientRect, GetCursorPos, GetWindowLongPtrW, HWND_TOPMOST, IDC_ARROW, LoadCursorW,
    PostMessageW, RegisterClassExW, SW_HIDE, SW_SHOW, SWP_SHOWWINDOW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_APP, WM_CLOSE, WM_CREATE, WM_DESTROY,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT,
    WS_CLIPCHILDREN, WS_EX_TOOLWINDOW, WS_POPUP, WNDCLASSEXW,
};

use crate::core::settings::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, PasteMode, Settings};
use crate::ui::gdi::{self, Fonts, MemDc, rect};
use crate::ui::theme::{Metrics, color};
use crate::win::strings::to_wide;

pub const CLASS_NAME: &str = "ClipHist.Settings";

/// 设置窗口 → 控制器：本地设置已改，来取走并应用。
pub const WM_SETTINGS_CHANGED: u32 = WM_APP + 30;
/// 设置窗口 → 控制器：清空历史。
pub const WM_SETTINGS_CLEAR: u32 = WM_APP + 31;
/// 设置窗口 → 控制器：打开数据文件所在目录。
pub const WM_SETTINGS_OPEN_FOLDER: u32 = WM_APP + 32;
/// 设置窗口 → 控制器：窗口已隐藏。
pub const WM_SETTINGS_CLOSED: u32 = WM_APP + 33;

/// 行 id（顺序即显示顺序）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Hotkey,
    MaxItems,
    KeepDays,
    MaxItemMb,
    MaxImageMb,
    PasteMode,
    Autostart,
    Pause,
    CaptureImages,
    DataPath,
    Clear,
}

pub const ROWS: [RowKind; 11] = [
    RowKind::Hotkey,
    RowKind::MaxItems,
    RowKind::KeepDays,
    RowKind::MaxItemMb,
    RowKind::MaxImageMb,
    RowKind::PasteMode,
    RowKind::Autostart,
    RowKind::Pause,
    RowKind::CaptureImages,
    RowKind::DataPath,
    RowKind::Clear,
];

/// 数值行的两半：左减右加。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Half {
    Decrease,
    Increase,
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub width: i32,
    pub header_h: i32,
    pub row_h: i32,
    pub footer_h: i32,
    pub pad: i32,
    /// 值区的左边界（标签占左侧固定宽度）
    pub value_x: i32,
}

impl Layout {
    pub fn new(dpi: u32) -> Self {
        let s = |v: i32| ((v as i64 * dpi as i64 + 48) / 96) as i32;
        Self {
            width: s(470),
            header_h: s(46),
            row_h: s(34),
            footer_h: s(32),
            pad: s(14),
            value_x: s(180),
        }
    }

    pub fn height(&self) -> i32 {
        self.header_h + self.row_h * ROWS.len() as i32 + self.footer_h
    }

    pub fn row_top(&self, index: usize) -> i32 {
        self.header_h + index as i32 * self.row_h
    }
}

/// y 坐标 → 行号（纯函数，可单测）。
pub fn row_at(l: &Layout, y: i32) -> Option<usize> {
    if y < l.header_h {
        return None;
    }
    let idx = ((y - l.header_h) / l.row_h) as usize;
    if idx < ROWS.len() { Some(idx) } else { None }
}

/// 鼠标落在值区的哪一半（纯函数，可单测）。
pub fn half_at(l: &Layout, x: i32) -> Half {
    let mid = l.value_x + (l.width - l.pad - l.value_x) / 2;
    if x < mid { Half::Decrease } else { Half::Increase }
}

/// 标题栏右上角的关闭按钮命中区。
pub fn close_button_rect(l: &Layout) -> RECT {
    rect(l.width - l.header_h, 0, l.width, l.header_h)
}

struct SettingsState {
    hwnd: HWND,
    owner: HWND,
    values: Settings,
    data_path: String,
    fonts: Fonts,
    layout: Layout,
    mem: Option<MemDc>,
    hover: Option<usize>,
    /// 热键捕获模式：下一次按键被当作新热键。
    capturing: bool,
    /// 清空历史的两段式确认。
    confirm_clear: bool,
    status: Option<String>,
}

impl SettingsState {
    fn new(hwnd: HWND, owner: HWND, values: Settings, data_path: String) -> Self {
        let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
        let dpi = if dpi == 0 { 96 } else { dpi };
        let metrics = Metrics::new(dpi);
        SettingsState {
            hwnd,
            owner,
            values,
            data_path,
            fonts: Fonts::create(&metrics),
            layout: Layout::new(dpi),
            mem: None,
            hover: None,
            capturing: false,
            confirm_clear: false,
            status: None,
        }
    }

    fn label(kind: RowKind) -> &'static str {
        match kind {
            RowKind::Hotkey => "弹出热键",
            RowKind::MaxItems => "最多保留",
            RowKind::KeepDays => "保留天数",
            RowKind::MaxItemMb => "单条上限",
            RowKind::MaxImageMb => "图片总量",
            RowKind::PasteMode => "选中后",
            RowKind::Autostart => "开机自启",
            RowKind::Pause => "暂停记录",
            RowKind::CaptureImages => "记录图片",
            RowKind::DataPath => "数据文件",
            RowKind::Clear => "清空历史",
        }
    }

    fn value(&self, kind: RowKind) -> String {
        let s = &self.values;
        match kind {
            RowKind::Hotkey => {
                if self.capturing {
                    "请按下新组合键…".to_string()
                } else {
                    s.hotkey_text()
                }
            }
            RowKind::MaxItems => {
                if s.max_items == 0 {
                    "不限 ◀▶".to_string()
                } else {
                    format!("◀ {} 条 ▶", s.max_items)
                }
            }
            RowKind::KeepDays => {
                if s.keep_days == 0 {
                    "不限 ◀▶".to_string()
                } else {
                    format!("◀ {} 天 ▶", s.keep_days)
                }
            }
            RowKind::MaxItemMb => format!("◀ {} MB ▶", s.max_item_bytes / (1024 * 1024)),
            RowKind::MaxImageMb => {
                format!("◀ {} MB ▶", s.max_image_bytes / (1024 * 1024))
            }
            RowKind::PasteMode => match s.paste_mode {
                PasteMode::AutoPaste => "自动粘贴".to_string(),
                PasteMode::ClipboardOnly => "仅放回剪贴板".to_string(),
            },
            RowKind::Autostart => on_off(s.autostart),
            RowKind::Pause => on_off(s.pause_capture),
            RowKind::CaptureImages => on_off(s.capture_images),
            RowKind::DataPath => self.data_path.clone(),
            RowKind::Clear => {
                if self.confirm_clear {
                    "再点一次确认清空（保留置顶）".to_string()
                } else {
                    "清空未置顶的记录".to_string()
                }
            }
        }
    }

    /// 是否是"值可点半边"的行。
    fn is_numeric(kind: RowKind) -> bool {
        matches!(
            kind,
            RowKind::MaxItems | RowKind::KeepDays | RowKind::MaxItemMb | RowKind::MaxImageMb
        )
    }

    fn step(&mut self, kind: RowKind, half: Half) {
        let dir = if half == Half::Increase { 1 } else { -1 };
        match kind {
            RowKind::MaxItems => {
                let step = if self.values.max_items >= 100 { 50 } else { 10 };
                let v = self.values.max_items + dir * step;
                self.values.max_items = v.clamp(0, 100_000);
            }
            RowKind::KeepDays => {
                let v = self.values.keep_days + dir;
                self.values.keep_days = v.clamp(0, 3650);
            }
            RowKind::MaxItemMb => {
                let cur = (self.values.max_item_bytes / (1024 * 1024)).max(1);
                let v = (cur + dir).clamp(1, 64);
                self.values.max_item_bytes = v * 1024 * 1024;
            }
            RowKind::MaxImageMb => {
                let cur = (self.values.max_image_bytes / (1024 * 1024)).max(10);
                let step = if cur >= 1000 { 500 } else { 50 };
                let v = (cur + dir * step).clamp(50, 8192);
                self.values.max_image_bytes = v * 1024 * 1024;
            }
            _ => return,
        }
        self.values.sanitize();
    }

    /// 返回是否改变了设置（需要通知控制器）。
    fn click_row(&mut self, index: usize, x: i32) -> ClickResult {
        let Some(kind) = ROWS.get(index).copied() else {
            return ClickResult::None;
        };
        if kind == RowKind::Clear {
            if self.confirm_clear {
                self.confirm_clear = false;
                return ClickResult::Clear;
            }
            self.confirm_clear = true;
            return ClickResult::Redraw;
        }
        if Self::is_numeric(kind) {
            let half = half_at(&self.layout, x);
            self.step(kind, half);
            return ClickResult::Changed;
        }
        match kind {
            RowKind::Hotkey => {
                self.capturing = true;
                ClickResult::Redraw
            }
            RowKind::PasteMode => {
                self.values.paste_mode = match self.values.paste_mode {
                    PasteMode::AutoPaste => PasteMode::ClipboardOnly,
                    PasteMode::ClipboardOnly => PasteMode::AutoPaste,
                };
                ClickResult::Changed
            }
            RowKind::Autostart => {
                self.values.autostart = !self.values.autostart;
                ClickResult::Changed
            }
            RowKind::Pause => {
                self.values.pause_capture = !self.values.pause_capture;
                ClickResult::Changed
            }
            RowKind::CaptureImages => {
                self.values.capture_images = !self.values.capture_images;
                ClickResult::Changed
            }
            RowKind::DataPath => ClickResult::OpenFolder,
            _ => ClickResult::None,
        }
    }

    /// 捕获热键。返回 true 表示这次按键被吃掉。
    fn capture_hotkey(&mut self, vk: u32) -> bool {
        if !self.capturing {
            return false;
        }
        if vk == 0x1B {
            // ESC 取消
            self.capturing = false;
            return true;
        }
        // 单独的修饰键不算，继续等真正的按键
        if matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C) {
            return true;
        }
        let mods = current_mods();
        if mods == 0 {
            // 没有修饰键的组合会抢走普通打字，拒绝并给出反馈
            self.status = Some("必须包含 Ctrl / Alt / Shift / Win".to_string());
            return true;
        }
        self.values.hotkey_mods = mods;
        self.values.hotkey_vk = vk;
        self.capturing = false;
        self.status = None;
        true
    }

    fn paint(&mut self, target: HDC) {
        let mut rc = rect(0, 0, 0, 0);
        unsafe { GetClientRect(self.hwnd, &mut rc) };
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        if w <= 0 || h <= 0 {
            return;
        }
        let need_new = match &self.mem {
            Some(mem) => mem.size() != (w, h),
            None => true,
        };
        if need_new {
            match &mut self.mem {
                Some(mem) => {
                    mem.resize(w, h);
                }
                None => self.mem = MemDc::new(w, h),
            }
        }
        match &self.mem {
            Some(mem) => {
                let dc = mem.dc();
                self.render(dc, w, h);
                mem.blit_to(target);
            }
            None => self.render(target, w, h),
        }
    }

    fn render(&self, dc: HDC, w: i32, h: i32) {
        let l = self.layout;
        gdi::fill(dc, &rect(0, 0, w, h), color::PAPER);
        // 标题
        let title = rect(l.pad, 0, w - l.header_h, l.header_h);
        gdi::text(
            dc,
            self.fonts.ui_bold,
            color::INK,
            &title,
            &format!("{} · 设置", crate::APP_TITLE),
            false,
        );
        gdi::hline(dc, 0, w, l.header_h - 1, color::RULE);

        // 关闭按钮
        let cb = close_button_rect(&l);
        gdi::text(dc, self.fonts.ui, color::INK2, &cb, "✕", true);

        for (i, kind) in ROWS.iter().enumerate() {
            let top = l.row_top(i);
            let row = rect(0, top, w, top + l.row_h);
            if self.hover == Some(i) {
                gdi::fill(dc, &row, color::HOVER);
            } else if i + 1 < ROWS.len() {
                gdi::hline(dc, l.pad, w - l.pad, row.bottom - 1, color::RULE);
            }
            let label = rect(l.pad, top, l.value_x - l.pad, top + l.row_h);
            let dim = kind == &RowKind::DataPath;
            gdi::text(
                dc,
                self.fonts.ui,
                if dim { color::INK3 } else { color::INK },
                &label,
                Self::label(*kind),
                false,
            );

            let is_path = *kind == RowKind::DataPath;
            let value = rect(l.value_x, top, w - l.pad, top + l.row_h);
            let f = if is_path { self.fonts.mono } else { self.fonts.ui };
            let vcolor = match kind {
                RowKind::Clear if self.confirm_clear => color::WARN,
                RowKind::Hotkey if self.capturing => color::WARN,
                RowKind::DataPath => color::INK3,
                _ => color::INK2,
            };
            gdi::text(dc, f, vcolor, &value, &self.value(*kind), false);
        }

        // 页脚
        let top = h - l.footer_h;
        gdi::hline(dc, 0, w, top, color::RULE);
        let footer = rect(l.pad, top, w - l.pad, h);
        let hint = self
            .status
            .clone()
            .unwrap_or_else(|| "单击切换 · 数值行点左右两半调整 · Esc 关闭".to_string());
        let c = if self.status.is_some() {
            color::WARN
        } else {
            color::INK3
        };
        gdi::text(dc, self.fonts.meta, c, &footer, &hint, false);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClickResult {
    None,
    Redraw,
    Changed,
    Clear,
    OpenFolder,
}

/// 当前修饰键状态（Win32 `MOD_*`）。
fn current_mods() -> u32 {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
    let down = |vk: i32| unsafe { GetKeyState(vk) } < 0;
    let mut m = 0;
    if down(0x11) {
        m |= MOD_CONTROL;
    }
    if down(0x12) {
        m |= MOD_ALT;
    }
    if down(0x10) {
        m |= MOD_SHIFT;
    }
    if down(0x5B) || down(0x5C) {
        m |= MOD_WIN;
    }
    m
}

pub fn register_class(hinstance: HINSTANCE) -> bool {
    let cls = to_wide(CLASS_NAME);
    let mut wc: WNDCLASSEXW = unsafe { zeroed() };
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.style = CS_HREDRAW | CS_VREDRAW;
    wc.lpfnWndProc = Some(wnd_proc);
    wc.hInstance = hinstance;
    wc.hCursor = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) };
    wc.hbrBackground = std::ptr::null_mut();
    wc.lpszClassName = cls.as_ptr();
    unsafe { RegisterClassExW(&wc) != 0 }
}

pub fn create(owner: HWND, hinstance: HINSTANCE) -> Option<HWND> {
    let cls = to_wide(CLASS_NAME);
    let title = to_wide("设置");
    let l = Layout::new(96);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            l.width,
            l.height(),
            owner,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() { None } else { Some(hwnd) }
}

unsafe fn state<'a>(hwnd: HWND) -> Option<&'a mut SettingsState> {
    let p = GetWindowLongPtrW(hwnd, windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA)
        as *mut SettingsState;
    if p.is_null() { None } else { Some(&mut *p) }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = lp as *const CREATESTRUCTW;
            let owner = if cs.is_null() {
                std::ptr::null_mut()
            } else {
                (*cs).hwndParent
            };
            // 真正的设置值由 show() 注入，这里先放默认值占位。
            let st = Box::new(SettingsState::new(hwnd, owner, Settings::default(), String::new()));
            SetWindowLongPtrW(
                hwnd,
                windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
                Box::into_raw(st) as isize,
            );
            return 1;
        }
        WM_CREATE => return 0,
        WM_DESTROY => {
            let p = GetWindowLongPtrW(hwnd, windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA)
                as *mut SettingsState;
            if !p.is_null() {
                SetWindowLongPtrW(
                    hwnd,
                    windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
                    0,
                );
                drop(Box::from_raw(p));
            }
            return 0;
        }
        WM_PAINT => {
            if let Some(st) = state(hwnd) {
                let mut ps: PAINTSTRUCT = zeroed();
                let dc = BeginPaint(hwnd, &mut ps);
                st.paint(dc);
                EndPaint(hwnd, &ps);
                return 0;
            }
            return 0;
        }
        WM_ERASEBKGND => return 1,
        WM_MOUSEMOVE => {
            if let Some(st) = state(hwnd) {
                let y = hiword(lp);
                let idx = row_at(&st.layout, y);
                if idx != st.hover {
                    st.hover = idx;
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
            }
            return 0;
        }
        WM_LBUTTONDOWN => {
            let mut out = ClickResult::None;
            let mut owner = std::ptr::null_mut();
            if let Some(st) = state(hwnd) {
                let x = loword(lp);
                let y = hiword(lp);
                owner = st.owner;
                // 关闭按钮
                let cb = close_button_rect(&st.layout);
                if x >= cb.left && x < cb.right && y >= cb.top && y < cb.bottom {
                    ShowWindow(hwnd, SW_HIDE);
                    if !owner.is_null() {
                        PostMessageW(owner, WM_SETTINGS_CLOSED, 0, 0);
                    }
                    return 0;
                }
                if let Some(idx) = row_at(&st.layout, y) {
                    out = st.click_row(idx, x);
                }
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            match out {
                ClickResult::Changed => {
                    if !owner.is_null() {
                        PostMessageW(owner, WM_SETTINGS_CHANGED, 0, 0);
                    }
                }
                ClickResult::Clear => {
                    if !owner.is_null() {
                        PostMessageW(owner, WM_SETTINGS_CLEAR, 0, 0);
                    }
                }
                ClickResult::OpenFolder => {
                    if !owner.is_null() {
                        PostMessageW(owner, WM_SETTINGS_OPEN_FOLDER, 0, 0);
                    }
                }
                _ => {}
            }
            return 0;
        }
        WM_KEYDOWN => {
            let vk = wp as u32;
            let mut changed = false;
            let mut hide = false;
            let mut owner = std::ptr::null_mut();
            if let Some(st) = state(hwnd) {
                owner = st.owner;
                if st.capturing {
                    changed = st.capture_hotkey(vk);
                    changed = changed && !st.capturing; // 真正改了热键才算变更
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                } else if vk == 0x1B {
                    hide = true;
                }
            }
            if hide {
                ShowWindow(hwnd, SW_HIDE);
                if !owner.is_null() {
                    PostMessageW(owner, WM_SETTINGS_CLOSED, 0, 0);
                }
            } else if changed && !owner.is_null() {
                PostMessageW(owner, WM_SETTINGS_CHANGED, 0, 0);
            }
            return 0;
        }
        WM_CLOSE => {
            ShowWindow(hwnd, SW_HIDE);
            let owner = state(hwnd).map(|st| st.owner).unwrap_or(std::ptr::null_mut());
            if !owner.is_null() {
                PostMessageW(owner, WM_SETTINGS_CLOSED, 0, 0);
            }
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// 显示设置窗口并注入当前值。
pub fn show(hwnd: HWND, values: Settings, data_path: &str) {
    if let Some(st) = unsafe { state(hwnd) } {
        st.values = values;
        st.data_path = data_path.to_string();
        st.capturing = false;
        st.confirm_clear = false;
        st.status = None;
        st.hover = None;
        let l = st.layout;
        let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut pt) };
        unsafe {
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                (pt.x - l.width / 2).max(0),
                (pt.y - l.height() / 3).max(0),
                l.width,
                l.height(),
                SWP_SHOWWINDOW,
            );
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        }
    }
}

/// 取回窗口里被改过的设置。
pub fn take_settings(hwnd: HWND) -> Option<Settings> {
    unsafe { state(hwnd).map(|st| st.values.clone()) }
}

pub fn is_visible(hwnd: HWND) -> bool {
    !hwnd.is_null() && unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd) } != 0
}

pub fn hide(hwnd: HWND) {
    unsafe { ShowWindow(hwnd, SW_HIDE) };
}

pub fn destroy(hwnd: HWND) {
    if !hwnd.is_null() {
        unsafe { DestroyWindow(hwnd) };
    }
}

/// 弹出/隐藏设置窗口（由控制器调用）。
pub fn toggle(hwnd: HWND, values: Settings, data_path: &str) {
    if is_visible(hwnd) {
        hide(hwnd);
    } else {
        show(hwnd, values, data_path);
    }
}

fn on_off(v: bool) -> String {
    if v { "开".to_string() } else { "关".to_string() }
}

fn loword(lp: LPARAM) -> i32 {
    (lp as u32 & 0xFFFF) as u16 as i16 as i32
}

fn hiword(lp: LPARAM) -> i32 {
    ((lp as u32 >> 16) & 0xFFFF) as u16 as i16 as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l() -> Layout {
        Layout::new(96)
    }

    #[test]
    fn layout_rows_are_contiguous_and_fit() {
        let l = l();
        assert_eq!(l.row_top(0), l.header_h);
        assert_eq!(l.row_top(1) - l.row_top(0), l.row_h);
        let last_bottom = l.row_top(ROWS.len() - 1) + l.row_h;
        assert!(last_bottom <= l.height() - l.footer_h + 1, "最后一行必须落在页脚之上");
        // 所有行的标签都不能压到值区
        assert!(l.value_x > l.pad && l.value_x < l.width);
    }

    #[test]
    fn row_hit_test() {
        let l = l();
        assert_eq!(row_at(&l, 0), None, "标题栏不是行");
        assert_eq!(row_at(&l, l.header_h), Some(0));
        assert_eq!(row_at(&l, l.header_h + l.row_h - 1), Some(0));
        assert_eq!(row_at(&l, l.header_h + l.row_h), Some(1));
        assert_eq!(row_at(&l, l.header_h + 2 * l.row_h + 3), Some(2));
        // 页脚不算行
        assert_eq!(row_at(&l, l.height() - 2), None);
    }

    #[test]
    fn numeric_half_split() {
        let l = l();
        let mid = l.value_x + (l.width - l.pad - l.value_x) / 2;
        assert_eq!(half_at(&l, l.value_x), Half::Decrease);
        assert_eq!(half_at(&l, mid - 1), Half::Decrease);
        assert_eq!(half_at(&l, mid), Half::Increase);
        assert_eq!(half_at(&l, l.width - l.pad - 1), Half::Increase);
    }

    #[test]
    fn close_button_is_top_right_and_inside() {
        let l = l();
        let cb = close_button_rect(&l);
        assert_eq!(cb.right, l.width);
        assert_eq!(cb.top, 0);
        assert!(cb.left > l.width / 2);
    }

    #[test]
    fn numeric_steps_clamp() {
        let mk = |v: Settings| {
            let mut st = SettingsState {
                hwnd: std::ptr::null_mut(),
                owner: std::ptr::null_mut(),
                values: v,
                data_path: String::new(),
                fonts: Fonts::create(&Metrics::new(96)),
                layout: Layout::new(96),
                mem: None,
                hover: None,
                capturing: false,
                confirm_clear: false,
                status: None,
            };
            st.step(RowKind::MaxItems, Half::Decrease);
            st.step(RowKind::MaxItems, Half::Decrease);
            st.step(RowKind::MaxItems, Half::Decrease);
            assert!(st.values.max_items >= 0);
            st.step(RowKind::MaxItems, Half::Increase);
            assert!(st.values.max_items > 0);

            st.step(RowKind::KeepDays, Half::Decrease);
            assert_eq!(st.values.keep_days, 29, "默认 30 天，减一次应当是 29");
            for _ in 0..40 {
                st.step(RowKind::KeepDays, Half::Decrease);
            }
            assert_eq!(st.values.keep_days, 0, "天数下限是 0（= 不限）");
            st.step(RowKind::KeepDays, Half::Decrease);
            assert_eq!(st.values.keep_days, 0, "到 0 之后必须保持 0，不能变负");

            st.values.max_item_bytes = 1024 * 1024;
            st.step(RowKind::MaxItemMb, Half::Decrease);
            assert_eq!(st.values.max_item_bytes, 1024 * 1024, "单条上限最小 1MB");

            st.values.max_image_bytes = 50 * 1024 * 1024;
            st.step(RowKind::MaxImageMb, Half::Decrease);
            assert_eq!(st.values.max_image_bytes, 50 * 1024 * 1024, "图片下限 50MB");
        };
        mk(Settings::default());
    }

    #[test]
    fn toggles_change_values() {
        let mut st = SettingsState {
            hwnd: std::ptr::null_mut(),
            owner: std::ptr::null_mut(),
            values: Settings::default(),
            data_path: String::new(),
            fonts: Fonts::create(&Metrics::new(96)),
            layout: Layout::new(96),
            mem: None,
            hover: None,
            capturing: false,
            confirm_clear: false,
            status: None,
        };
        let pause_idx = ROWS.iter().position(|r| *r == RowKind::Pause).unwrap();
        assert_eq!(st.click_row(pause_idx, 300), ClickResult::Changed);
        assert!(st.values.pause_capture);
        assert_eq!(st.click_row(pause_idx, 300), ClickResult::Changed);
        assert!(!st.values.pause_capture);

        let paste_idx = ROWS.iter().position(|r| *r == RowKind::PasteMode).unwrap();
        assert_eq!(st.click_row(paste_idx, 300), ClickResult::Changed);
        assert_eq!(st.values.paste_mode, PasteMode::ClipboardOnly);

        // 热键行进入捕获模式，但不算"设置变更"
        let hk_idx = ROWS.iter().position(|r| *r == RowKind::Hotkey).unwrap();
        assert_eq!(st.click_row(hk_idx, 300), ClickResult::Redraw);
        assert!(st.capturing);
        // 单独的 Ctrl 不算
        assert!(st.capture_hotkey(0x11));
        assert!(st.capturing);
        // ESC 取消
        assert!(st.capture_hotkey(0x1B));
        assert!(!st.capturing);
    }

    #[test]
    fn clear_requires_two_clicks() {
        let mut st = SettingsState {
            hwnd: std::ptr::null_mut(),
            owner: std::ptr::null_mut(),
            values: Settings::default(),
            data_path: String::new(),
            fonts: Fonts::create(&Metrics::new(96)),
            layout: Layout::new(96),
            mem: None,
            hover: None,
            capturing: false,
            confirm_clear: false,
            status: None,
        };
        let idx = ROWS.iter().position(|r| *r == RowKind::Clear).unwrap();
        assert_eq!(st.click_row(idx, 300), ClickResult::Redraw, "第一次只是进入确认态");
        assert!(st.confirm_clear);
        assert_eq!(st.click_row(idx, 300), ClickResult::Clear, "第二次才真的清空");
        assert!(!st.confirm_clear);
    }

    #[test]
    fn value_text_reflects_settings() {
        let st = SettingsState {
            hwnd: std::ptr::null_mut(),
            owner: std::ptr::null_mut(),
            values: Settings::default(),
            data_path: r"C:\x\data.db".to_string(),
            fonts: Fonts::create(&Metrics::new(96)),
            layout: Layout::new(96),
            mem: None,
            hover: None,
            capturing: false,
            confirm_clear: false,
            status: None,
        };
        assert_eq!(st.value(RowKind::Hotkey), "Alt+V");
        assert_eq!(st.value(RowKind::MaxItems), "◀ 500 条 ▶");
        assert_eq!(st.value(RowKind::KeepDays), "◀ 30 天 ▶");
        assert_eq!(st.value(RowKind::MaxItemMb), "◀ 2 MB ▶");
        assert_eq!(st.value(RowKind::MaxImageMb), "◀ 200 MB ▶");
        assert_eq!(st.value(RowKind::PasteMode), "自动粘贴");
        assert_eq!(st.value(RowKind::Autostart), "关");
        assert_eq!(st.value(RowKind::CaptureImages), "开");
        assert_eq!(st.value(RowKind::DataPath), r"C:\x\data.db");
    }
}
