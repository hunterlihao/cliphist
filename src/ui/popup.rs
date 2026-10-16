//! 历史弹窗：搜索框 + 自绘列表 + 键盘/鼠标交互。
//!
//! 分工：
//! - 本模块只负责**显示与交互**，不碰数据库、不碰粘贴逻辑；选中后向控制器窗口
//!   `PostMessage`，由 [`crate::app`] 去做真正的落库/粘贴。这样弹窗里没有任何会泵消息
//!   的调用（没有 `TrackPopupMenu`、没有模态框），它的窗口过程不可能被重入，
//!   因此可以安全地用 `GWLP_USERDATA` 保存 `Box<PopupState>` 裸指针。
//! - 右键菜单由控制器弹出（菜单会泵消息），正好躲开上面这个约束。

use std::collections::HashSet;
use std::mem::zeroed;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, GetMonitorInfoW, MonitorFromPoint,
    HBRUSH, HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, WHITE_BRUSH,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos,
    GetParent, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
    LoadCursorW, MoveWindow, PostMessageW, RegisterClassExW, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, ES_AUTOHSCROLL, ES_LEFT, HWND_TOPMOST, IDC_ARROW, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW,
    WM_ACTIVATE, WM_APP, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT, WM_DESTROY,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_PAINT,
    WM_RBUTTONUP, WM_SETFONT, WM_SYSKEYDOWN, WNDCLASSEXW, WNDPROC, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::core::model::{ItemRow, Kind};
use crate::ui::gdi::{self, rect, Fonts, MemDc, ThumbCache};
use crate::ui::theme::{color, Metrics};
use crate::ui::timefmt;
use crate::win::strings::{from_wide_until_nul, to_wide};

pub const CLASS_NAME: &str = "ClipHist.Popup";
const EDIT_CLASS: &str = "EDIT";
const EDIT_ID: usize = 101;

/// 弹窗 → 控制器：搜索内容变化，请重新查询并回填。
pub const WM_POPUP_QUERY: u32 = WM_APP + 10;
/// 弹窗 → 控制器：用户选中某条。`wParam` = 1 表示"只放回剪贴板不自动粘贴"。
pub const WM_POPUP_CHOOSE: u32 = WM_APP + 11;
/// 弹窗 → 控制器：请求对某条弹右键菜单（菜单必须由控制器弹）。
pub const WM_POPUP_CONTEXT: u32 = WM_APP + 12;
/// 弹窗 → 控制器：弹窗已关闭。
pub const WM_POPUP_CLOSED: u32 = WM_APP + 13;
/// 弹窗 → 控制器：需要某条的缩略图（lParam = item id）。
pub const WM_POPUP_NEED_THUMB: u32 = WM_APP + 14;
/// 弹窗 → 控制器：切换某条的置顶状态。
pub const WM_POPUP_PIN: u32 = WM_APP + 15;
/// 弹窗 → 控制器：删除某条。
pub const WM_POPUP_DELETE: u32 = WM_APP + 16;

const EN_CHANGE: usize = 0x0300;
const VK_RETURN: usize = 0x0D;
const VK_ESCAPE: usize = 0x1B;
const VK_UP: usize = 0x26;
const VK_DOWN: usize = 0x28;
const VK_PRIOR: usize = 0x21;
const VK_NEXT: usize = 0x22;
const VK_HOME: usize = 0x24;
const VK_END: usize = 0x23;
const VK_CONTROL: i32 = 0x11;
const VK_SHIFT: i32 = 0x10;

/// 列表导航状态（纯数据，便于单测）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListNav {
    pub total: usize,
    pub selected: usize,
    /// 顶部行索引
    pub top: usize,
}

impl ListNav {
    pub fn new(total: usize) -> Self {
        Self {
            total,
            selected: 0,
            top: 0,
        }
    }

    /// 可视行数。
    pub fn visible(&self, m: &Metrics) -> usize {
        m.visible_rows() as usize
    }

    pub fn max_top(&self, m: &Metrics) -> usize {
        self.total.saturating_sub(self.visible(m))
    }

    pub fn clamp(&mut self, m: &Metrics) {
        if self.total == 0 {
            self.selected = 0;
            self.top = 0;
            return;
        }
        self.selected = self.selected.min(self.total - 1);
        self.top = self.top.min(self.max_top(m));
        self.ensure_visible(m);
    }

    pub fn ensure_visible(&mut self, m: &Metrics) {
        let vis = self.visible(m);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + vis {
            self.top = self.selected + 1 - vis;
        }
        self.top = self.top.min(self.max_top(m));
    }

    pub fn select(&mut self, m: &Metrics, idx: usize) {
        self.selected = idx.min(self.total.saturating_sub(1));
        self.ensure_visible(m);
    }

    pub fn move_by(&mut self, m: &Metrics, delta: i32) {
        if self.total == 0 {
            return;
        }
        let cur = self.selected as i32;
        let next = (cur + delta).clamp(0, self.total as i32 - 1);
        self.selected = next as usize;
        self.ensure_visible(m);
    }

    pub fn page(&mut self, m: &Metrics, pages: i32) {
        let step = self.visible(m) as i32 * pages;
        self.move_by(m, step);
    }

    pub fn scroll_rows(&mut self, m: &Metrics, rows: i32) {
        let next = (self.top as i32 + rows).clamp(0, self.max_top(m) as i32);
        self.top = next as usize;
        // 滚动后让选择落在可视区内（不改变选择值本身）
        let vis = self.visible(m);
        if self.selected < self.top {
            self.selected = self.top;
        } else if self.selected >= self.top + vis {
            self.selected = (self.top + vis - 1).min(self.total.saturating_sub(1));
        }
    }

    /// 当前可视区间的起止（左闭右开）。
    pub fn range(&self, m: &Metrics) -> (usize, usize) {
        let start = self.top.min(self.total);
        let end = (self.top + self.visible(m)).min(self.total);
        (start, end)
    }
}

/// 点击 y 坐标 → 行号。
pub fn row_at(m: &Metrics, top: usize, y: i32) -> Option<usize> {
    if y < m.search_h {
        return None;
    }
    let list_h = m.list_height();
    if y >= m.search_h + list_h {
        return None;
    }
    let idx = top + ((y - m.search_h) / m.row_h) as usize;
    Some(idx)
}

/// 单行矩形。
pub fn row_rect(m: &Metrics, top: usize, index: usize) -> RECT {
    let y0 = m.search_h + (index as i32 - top as i32) * m.row_h;
    rect(0, y0, m.width, y0 + m.row_h)
}

/// 键盘动作（由窗口过程翻译成消息）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    None,
    /// 选中第 N 条（Ctrl+1..9）
    PickIndex(usize),
    Choose {
        clipboard_only: bool,
    },
    Context,
    Pin,
    Delete,
    Close,
    Consumed,
}

/// 键盘处理（纯逻辑，可单测）：返回动作 + 是否需要重绘。
pub fn handle_key(
    nav: &mut ListNav,
    m: &Metrics,
    vk: usize,
    ctrl: bool,
    shift: bool,
) -> (KeyAction, bool) {
    match vk {
        VK_ESCAPE => (KeyAction::Close, false),
        VK_RETURN => (
            KeyAction::Choose {
                clipboard_only: shift,
            },
            false,
        ),
        VK_UP => {
            nav.move_by(m, -1);
            (KeyAction::Consumed, true)
        }
        VK_DOWN => {
            nav.move_by(m, 1);
            (KeyAction::Consumed, true)
        }
        VK_PRIOR => {
            nav.page(m, -1);
            (KeyAction::Consumed, true)
        }
        VK_NEXT => {
            nav.page(m, 1);
            (KeyAction::Consumed, true)
        }
        VK_HOME => {
            nav.select(m, 0);
            (KeyAction::Consumed, true)
        }
        VK_END => {
            nav.select(m, nav.total.saturating_sub(1));
            (KeyAction::Consumed, true)
        }
        0x31..=0x39 if ctrl => {
            // Ctrl+1..9 直选
            let idx = vk - 0x31;
            if idx < nav.total {
                nav.select(m, idx);
                (KeyAction::PickIndex(idx), true)
            } else {
                (KeyAction::Consumed, false)
            }
        }
        0x50 if ctrl => (KeyAction::Pin, false), // Ctrl+P 置顶
        0x44 if ctrl => (KeyAction::Delete, false), // Ctrl+D 删除
        _ => (KeyAction::None, false),
    }
}

/// 弹窗状态。生命周期与窗口一致。
struct PopupState {
    hwnd: HWND,
    owner: HWND,
    edit: HWND,
    edit_old_proc: isize,
    fonts: Fonts,
    metrics: Metrics,
    mem: Option<MemDc>,
    nav: ListNav,
    rows: Vec<ItemRow>,
    hover: Option<usize>,
    thumbs: ThumbCache,
    /// 已经向控制器请求过缩略图的 id，避免每帧重复请求。
    requested_thumbs: HashSet<i64>,
    status: Option<String>,
    well_brush: HBRUSH,
    query: String,
}

impl PopupState {
    fn new(hwnd: HWND, owner: HWND) -> PopupState {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let metrics = Metrics::new(if dpi == 0 { 96 } else { dpi });
        PopupState {
            hwnd,
            owner,
            edit: std::ptr::null_mut(),
            edit_old_proc: 0,
            fonts: Fonts::create(&metrics),
            metrics,
            mem: None,
            nav: ListNav::new(0),
            rows: Vec::new(),
            hover: None,
            thumbs: ThumbCache::new(24),
            requested_thumbs: HashSet::new(),
            status: None,
            well_brush: std::ptr::null_mut(),
            query: String::new(),
        }
    }

    /// 创建搜索框子控件。用系统 EDIT 而不是自绘：中文输入法、光标、选区、
    /// 右键菜单全都白送，自绘一个文本框去重实现这些纯属自找麻烦。
    fn create_edit(&mut self) {
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let cls = to_wide(EDIT_CLASS);
        let empty = to_wide("");
        let style = WS_CHILD | WS_VISIBLE | ES_LEFT as u32 | ES_AUTOHSCROLL as u32;
        let h = unsafe {
            CreateWindowExW(
                0,
                cls.as_ptr(),
                empty.as_ptr(),
                style,
                0,
                0,
                10,
                10,
                self.hwnd,
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            )
        };
        if h.is_null() {
            return;
        }
        self.edit = h;
        // 换字体
        let font = self.fonts.ui;
        unsafe { SendMessageW(h, WM_SETFONT, font as usize, 1) };
        // 子类化：方向键/回车/Esc 必须由我们处理，否则会被 EDIT 吃掉或响铃。
        let old = unsafe {
            SetWindowLongPtrW(
                h,
                windows_sys::Win32::UI::WindowsAndMessaging::GWLP_WNDPROC,
                edit_proc as *const () as usize as isize,
            )
        };
        self.edit_old_proc = old;
        self.layout_edit();
    }

    /// 搜索框位置：置于顶部条内，留出左右内边距。
    fn layout_edit(&self) {
        if self.edit.is_null() {
            return;
        }
        let m = &self.metrics;
        let line = m.fs_preview + 8;
        let top = (m.search_h - line) / 2;
        unsafe {
            MoveWindow(
                self.edit,
                m.pad + 4,
                top,
                (m.width - 2 * m.pad - 8).max(10),
                line,
                1,
            )
        };
    }

    fn set_rows(&mut self, rows: Vec<ItemRow>) {
        self.rows = rows;
        self.nav = ListNav::new(self.rows.len());
        self.nav.clamp(&self.metrics);
        self.hover = None;
        // 缩略图缓存按 id 索引且由 LRU 限界，跨刷新保留即可（反复打开弹窗不必重新解码）。
    }

    fn current_id(&self) -> Option<i64> {
        self.rows.get(self.nav.selected).map(|r| r.id)
    }

    /// 让窗口高度贴合内容，并在光标附近定位（自动避开屏幕边缘）。
    fn place(&mut self) {
        let m = self.metrics;
        let h = m.height_for(self.rows.len());
        let mut pt = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut pt) };
        let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
        let mut mi: MONITORINFO = unsafe { zeroed() };
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let ok = unsafe { GetMonitorInfoW(mon, &mut mi) };
        let work = if ok != 0 {
            mi.rcWork
        } else {
            rect(0, 0, 1920, 1080)
        };

        let gap = m.pad;
        let mut x = pt.x - m.width / 2;
        x = x.clamp(
            work.left + gap,
            (work.right - m.width - gap).max(work.left + gap),
        );
        let mut y = pt.y + gap;
        if y + h > work.bottom - gap {
            // 下方放不下就翻到光标上方
            y = pt.y - h - gap;
        }
        y = y.clamp(work.top + gap, (work.bottom - h - gap).max(work.top + gap));

        unsafe { SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, m.width, h, SWP_SHOWWINDOW) };
    }

    // ---------------------------------------------------------- 绘制

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

        // 先把内存 DC 取出来（take），这样在 render 里可以自由使用 &mut self，
        // 画完再放回去 —— 直接持有 &self.mem 会和 &mut self 冲突。
        let Some(mut mem) = self.mem.take() else {
            // 内存 DC 建不出来（极端情况）就直接画到屏幕，宁可闪也别白屏。
            self.render(target, w, h);
            return;
        };
        if !mem.resize(w, h) {
            self.mem = Some(mem);
            self.render(target, w, h);
            return;
        }
        let dc = mem.dc();
        self.render(dc, w, h);
        mem.blit_to(target);
        self.mem = Some(mem);
    }

    fn render(&mut self, dc: HDC, w: i32, h: i32) {
        let paper = rect(0, 0, w, h);
        gdi::fill(dc, &paper, color::PAPER);

        self.render_header(dc, w);
        self.render_list(dc, w);
        self.render_footer(dc, w, h);
        // 外框：WS_POPUP 没有非客户区，靠这一圈发丝线跟桌面分开
        gdi::frame(dc, &paper, color::RULE_STRONG);
    }

    fn render_header(&self, dc: HDC, w: i32) {
        let m = self.metrics;
        let header = rect(1, 1, w - 1, m.search_h);
        gdi::fill(dc, &header, color::WELL);
        gdi::hline(dc, 1, w - 1, m.search_h - 1, color::RULE);
    }

    fn render_list(&mut self, dc: HDC, w: i32) {
        let m = self.metrics;
        if self.rows.is_empty() {
            let msg = if self.query.is_empty() {
                "还没有历史记录"
            } else {
                "没有匹配的记录"
            };
            let area = rect(0, m.search_h, w, m.search_h + m.list_height());
            gdi::text(dc, self.fonts.meta, color::INK3, &area, msg, true);
            return;
        }

        let (start, end) = self.nav.range(&m);
        let now = crate::core::store::now_ms();

        for idx in start..end {
            let row = self.rows[idx].clone();
            let rc = row_rect(&m, self.nav.top, idx);
            let selected = idx == self.nav.selected;
            let hovered = self.hover == Some(idx);

            if selected {
                gdi::fill(dc, &rc, color::SEL_BG);
            } else if hovered {
                gdi::fill(dc, &rc, color::HOVER);
            } else if idx + 1 < end {
                gdi::hline(dc, m.pad, w - m.pad, rc.bottom - 1, color::RULE);
            }

            let fg = if selected { color::SEL_FG } else { color::INK };
            let fg2 = if selected {
                color::SEL_FG2
            } else {
                color::INK2
            };
            let fg3 = if selected {
                color::SEL_FG2
            } else {
                color::INK3
            };

            // 左侧方块：图片用它画缩略图，文本/文件画类型徽标
            let thumb_top = rc.top + (m.row_h - m.thumb) / 2;
            let box_rc = rect(m.pad, thumb_top, m.pad + m.thumb, thumb_top + m.thumb);
            let badge_bg = if selected {
                color::SEL_FG2
            } else {
                color::BADGE_BG
            };
            let badge_fg = if selected {
                color::SEL_BG
            } else {
                color::BADGE_FG
            };

            if row.kind == Kind::Image && self.thumbs.contains(row.id) {
                self.thumbs.draw(dc, row.id, &box_rc);
            } else {
                gdi::round_fill(dc, &box_rc, badge_bg, m.radius);
                let label = match row.kind {
                    Kind::Text => "文本",
                    Kind::Files => "文件",
                    Kind::Image => "图片",
                };
                gdi::text(dc, self.fonts.badge, badge_fg, &box_rc, label, true);
                if row.kind == Kind::Image && !self.requested_thumbs.contains(&row.id) {
                    // 懒加载：只对真正显示出来的图片行要缩略图。
                    self.requested_thumbs.insert(row.id);
                    unsafe { PostMessageW(self.owner, WM_POPUP_NEED_THUMB, 0, row.id as isize) };
                }
            }

            // 文本区
            let text_left = m.pad + m.thumb + m.gap;
            let pin_w = m.gap * 2 + 6;
            let text_right = w - m.pad - pin_w;
            let line_h = gdi::text_height(dc, self.fonts.ui).max(m.fs_preview);
            let meta_h = gdi::text_height(dc, self.fonts.meta).max(m.fs_meta);
            let content_h = line_h + meta_h + 2;
            let top = rc.top + (m.row_h - content_h) / 2;

            let preview_rc = rect(text_left, top, text_right, top + line_h);
            gdi::text(dc, self.fonts.ui, fg, &preview_rc, &row.preview, false);

            // 元信息：类型补充 · 来源进程 · 相对时间
            let mut parts: Vec<String> = Vec::new();
            match row.kind {
                Kind::Files if row.file_count > 0 => {
                    parts.push(format!("{} 个文件", row.file_count))
                }
                Kind::Image if row.img_w > 0 && row.img_h > 0 => {
                    parts.push(format!("{}×{}", row.img_w, row.img_h))
                }
                _ => {}
            }
            if let Some(src) = &row.source_exe {
                parts.push(src.clone());
            }
            parts.push(timefmt::relative(now, row.last_used_at));

            let meta_rc = rect(
                text_left,
                top + line_h + 2,
                text_right,
                top + line_h + 2 + meta_h,
            );
            gdi::text(
                dc,
                self.fonts.meta,
                fg2,
                &meta_rc,
                &parts.join(" · "),
                false,
            );

            // 右侧：使用次数（等宽，便于竖着对齐）
            if row.use_count > 1 {
                let s = format!("×{}", row.use_count + 1);
                let wd = gdi::text_width(dc, self.fonts.mono, &s);
                let trc = rect(text_right - wd, rc.top, text_right, rc.bottom);
                gdi::text(dc, self.fonts.mono, fg3, &trc, &s, false);
            }

            // 置顶标记
            if row.pinned {
                let cx = w - m.pad - 4;
                let cy = rc.top + m.row_h / 2;
                let r = (m.dpi as i32 / 48).max(2);
                gdi::dot(
                    dc,
                    cx,
                    cy,
                    r,
                    if selected { color::SEL_FG } else { color::PIN },
                );
            }
        }

        self.render_scrollbar(dc, w);
    }

    fn render_scrollbar(&self, dc: HDC, w: i32) {
        let m = self.metrics;
        let vis = self.nav.visible(&m);
        if self.rows.len() <= vis {
            return;
        }
        let list_top = m.search_h + 2;
        let list_h = m.list_height() - 4;
        let track_x = w - m.pad / 2 - m.scrollbar_w;
        gdi::fill(
            dc,
            &rect(
                track_x,
                list_top,
                track_x + m.scrollbar_w,
                list_top + list_h,
            ),
            color::RULE,
        );
        let total = self.rows.len().max(1);
        let thumb_h = ((list_h as i64 * vis as i64) / total as i64).max(m.row_h as i64 / 2) as i32;
        let max_top = self.nav.max_top(&m).max(1) as i64;
        let y = list_top + (((list_h - thumb_h) as i64 * self.nav.top as i64) / max_top) as i32;
        gdi::fill(
            dc,
            &rect(track_x, y, track_x + m.scrollbar_w, y + thumb_h),
            color::INK3,
        );
    }

    fn render_footer(&self, dc: HDC, w: i32, h: i32) {
        let m = self.metrics;
        let top = h - m.footer_h;
        if top <= m.search_h {
            return;
        }
        gdi::hline(dc, 1, w - 1, top, color::RULE);
        let hint = "↑↓ 选择 · Enter 粘贴 · Shift+Enter 仅复制 · Ctrl+P 置顶 · Esc 关闭";
        let status = self
            .status
            .clone()
            .unwrap_or_else(|| format!("共 {} 条", self.rows.len()));
        let sw = gdi::text_width(dc, self.fonts.mono, &status);
        let hint_rc = rect(m.pad, top, w - m.pad - sw - m.gap, h);
        gdi::text(dc, self.fonts.meta, color::INK3, &hint_rc, hint, false);
        let s_rc = rect(w - m.pad - sw, top, w - m.pad, h);
        gdi::text(dc, self.fonts.mono, color::INK3, &s_rc, &status, false);
    }
}

// ------------------------------------------------------------- 窗口过程

pub fn register_class(hinstance: HINSTANCE) -> bool {
    let cls = to_wide(CLASS_NAME);
    let mut wc: WNDCLASSEXW = unsafe { zeroed() };
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.style = CS_HREDRAW | CS_VREDRAW;
    wc.lpfnWndProc = Some(wnd_proc);
    wc.hInstance = hinstance;
    wc.hCursor = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) };
    // 背景刷留空：整个客户区我们自己画，交给系统刷只会闪。
    wc.hbrBackground = std::ptr::null_mut();
    wc.lpszClassName = cls.as_ptr();
    // RegisterClassExW 返回 ATOM(u16)，0 表示失败。
    unsafe { RegisterClassExW(&wc) != 0 }
}

/// 创建弹窗（初始隐藏）。`owner` 为控制器窗口。
pub fn create(owner: HWND, hinstance: HINSTANCE) -> Option<HWND> {
    let cls = to_wide(CLASS_NAME);
    let title = to_wide(crate::APP_TITLE);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            420,
            300,
            owner,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        None
    } else {
        Some(hwnd)
    }
}

unsafe fn state<'a>(hwnd: HWND) -> Option<&'a mut PopupState> {
    let p = GetWindowLongPtrW(
        hwnd,
        windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
    ) as *mut PopupState;
    if p.is_null() {
        None
    } else {
        Some(&mut *p)
    }
}

/// 消息处理的结论。**先算结论、再动作**，是这个文件最重要的约定：
///
/// `ShowWindow(SW_HIDE)` 会**同步**派发消息（WM_ACTIVATE / WM_KILLFOCUS / WM_WINDOWPOSCHANGED），
/// 若此时仍握着 `&mut PopupState`，同一次调用栈里就会产生第二个 `&mut` —— 那是 UB。
/// 所以凡是会派发或泵消息的动作（只有 `hide`）一律推迟到借用结束之后执行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    None,
    Redraw,
    Close,
    Choose { id: i64, clipboard_only: bool },
    Context { id: i64 },
    Pin { id: i64 },
    Delete { id: i64 },
}

/// 执行结论（此时不应再持有任何 `&mut PopupState`）。
unsafe fn dispatch(hwnd: HWND, out: Outcome) {
    if out == Outcome::None {
        return;
    }
    let owner = state(hwnd)
        .map(|st| st.owner)
        .unwrap_or(std::ptr::null_mut());
    match out {
        Outcome::None => {}
        Outcome::Redraw => invalidate(hwnd),
        Outcome::Close => hide(hwnd),
        Outcome::Choose { id, clipboard_only } => {
            hide(hwnd);
            if !owner.is_null() {
                PostMessageW(owner, WM_POPUP_CHOOSE, clipboard_only as usize, id as isize);
            }
        }
        Outcome::Context { id } => {
            if !owner.is_null() {
                PostMessageW(owner, WM_POPUP_CONTEXT, 0, id as isize);
            }
        }
        Outcome::Pin { id } => {
            if !owner.is_null() {
                PostMessageW(owner, WM_POPUP_PIN, 0, id as isize);
            }
        }
        Outcome::Delete { id } => {
            if !owner.is_null() {
                PostMessageW(owner, WM_POPUP_DELETE, 0, id as isize);
            }
        }
    }
}

/// 把按键动作翻译成结论。
fn outcome_for(st: &PopupState, action: KeyAction, repaint: bool) -> Outcome {
    let id = st.current_id();
    match action {
        KeyAction::None | KeyAction::Consumed => {
            if repaint {
                Outcome::Redraw
            } else {
                Outcome::None
            }
        }
        KeyAction::Close => Outcome::Close,
        KeyAction::Choose { clipboard_only } => match id {
            Some(id) => Outcome::Choose { id, clipboard_only },
            None => Outcome::None,
        },
        KeyAction::Context => match id {
            Some(id) => Outcome::Context { id },
            None => Outcome::None,
        },
        KeyAction::Pin => match id {
            Some(id) => Outcome::Pin { id },
            None => Outcome::None,
        },
        KeyAction::Delete => match id {
            Some(id) => Outcome::Delete { id },
            None => Outcome::None,
        },
        // Ctrl+1..9 直选：立刻粘贴
        KeyAction::PickIndex(_) => match id {
            Some(id) => Outcome::Choose {
                id,
                clipboard_only: false,
            },
            None => Outcome::None,
        },
    }
}

/// 在**最短借用**内处理按键，返回（结论, 是否应当放行给搜索框）。
unsafe fn key_outcome(hwnd: HWND, vk: usize) -> (Outcome, bool) {
    let ctrl = is_down(VK_CONTROL);
    let shift = is_down(VK_SHIFT);
    if let Some(st) = state(hwnd) {
        let (action, repaint) = handle_key(&mut st.nav, &st.metrics, vk, ctrl, shift);
        let passthrough = action == KeyAction::None;
        (outcome_for(st, action, repaint), passthrough)
    } else {
        (Outcome::None, true)
    }
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
            let state = Box::new(PopupState::new(hwnd, owner));
            SetWindowLongPtrW(
                hwnd,
                windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
                Box::into_raw(state) as isize,
            );
            return 1; // 继续创建
        }
        WM_CREATE => {
            if let Some(st) = state(hwnd) {
                st.well_brush = CreateSolidBrush(color::WELL);
                st.create_edit();
            }
            return 0;
        }
        WM_DESTROY => {
            let p = GetWindowLongPtrW(
                hwnd,
                windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            ) as *mut PopupState;
            if !p.is_null() {
                SetWindowLongPtrW(
                    hwnd,
                    windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
                    0,
                );
                let st = Box::from_raw(p);
                if !st.well_brush.is_null() {
                    DeleteObject(st.well_brush as windows_sys::Win32::Graphics::Gdi::HGDIOBJ);
                }
                drop(st);
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
        }
        WM_ERASEBKGND => return 1, // 双缓冲自己负责，禁止系统擦背景（防闪）
        WM_CTLCOLOREDIT => {
            if let Some(st) = state(hwnd) {
                // 搜索框融进顶部条：底色用 WELL，与周围一致。
                windows_sys::Win32::Graphics::Gdi::SetTextColor(wp as HDC, color::INK);
                windows_sys::Win32::Graphics::Gdi::SetBkColor(wp as HDC, color::WELL);
                if !st.well_brush.is_null() {
                    return st.well_brush as isize;
                }
            }
            return unsafe { windows_sys::Win32::Graphics::Gdi::GetStockObject(WHITE_BRUSH) }
                as isize;
        }
        WM_COMMAND => {
            let id = wp & 0xFFFF;
            let code = (wp >> 16) & 0xFFFF;
            if id == EDIT_ID && code == EN_CHANGE {
                if let Some(st) = state(hwnd) {
                    let q = read_edit_text(st.edit);
                    if q != st.query {
                        st.query = q;
                        PostMessageW(st.owner, WM_POPUP_QUERY, 0, 0);
                    }
                }
            }
            return 0;
        }
        WM_ACTIVATE => {
            // 点别处 / 切走即关闭（Win+V 的手感）。先判可见性，避免 ShowWindow 递归回来时反复 hide。
            if (wp & 0xFFFF) as u32 == 0 && is_visible(hwnd) {
                dispatch(hwnd, Outcome::Close);
            }
            return 0;
        }
        WM_MOUSEMOVE => {
            if let Some(st) = state(hwnd) {
                let y = hiword(lp);
                let idx = row_at(&st.metrics, st.nav.top, y).filter(|i| *i < st.rows.len());
                if idx != st.hover {
                    st.hover = idx;
                    invalidate(hwnd);
                }
            }
            return 0;
        }
        WM_LBUTTONDOWN => {
            // 单击即粘贴（与 Win+V 一致）。
            let out = if let Some(st) = state(hwnd) {
                let y = hiword(lp);
                match row_at(&st.metrics, st.nav.top, y) {
                    Some(idx) if idx < st.rows.len() => {
                        st.nav.select(&st.metrics, idx);
                        st.hover = Some(idx);
                        let id = st.rows[idx].id;
                        Outcome::Choose {
                            id,
                            clipboard_only: is_down(VK_SHIFT),
                        }
                    }
                    _ => Outcome::None,
                }
            } else {
                Outcome::None
            };
            dispatch(hwnd, out);
            return 0;
        }
        WM_RBUTTONUP => {
            let mut ctx: Option<i64> = None;
            if let Some(st) = state(hwnd) {
                let y = hiword(lp);
                if let Some(idx) = row_at(&st.metrics, st.nav.top, y) {
                    if idx < st.rows.len() {
                        st.nav.select(&st.metrics, idx);
                        invalidate(hwnd);
                        ctx = st.rows.get(idx).map(|r| r.id);
                    }
                }
            }
            if let Some(st) = state(hwnd) {
                if let Some(id) = ctx {
                    // 菜单交给控制器弹：那里可以安全地泵消息。
                    PostMessageW(st.owner, WM_POPUP_CONTEXT, 0, id as isize);
                }
            }
            return 0;
        }
        WM_MOUSEWHEEL => {
            if let Some(st) = state(hwnd) {
                let delta = ((wp >> 16) & 0xFFFF) as u16 as i16 as i32;
                let rows = if delta > 0 { -3 } else { 3 };
                st.nav.scroll_rows(&st.metrics, rows);
                invalidate(hwnd);
            }
            return 0;
        }
        WM_CLOSE => {
            hide(hwnd);
            return 0;
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let (out, passthrough) = key_outcome(hwnd, wp);
            if passthrough {
                // 字母/退格这类键交回默认处理（焦点在弹窗上时不影响搜索框）
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            dispatch(hwnd, out);
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// 搜索框子类化过程：把导航键转给弹窗处理，其余原样交给 EDIT。
///
/// 这里同样只在最短时间内借用状态：`key_outcome` 借完即还，
/// `edit_old_proc` 是裸指针读，不构成借用。
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let parent = GetParent(hwnd);
    if parent.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    let ptr = GetWindowLongPtrW(
        parent,
        windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
    ) as *mut PopupState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }

    if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
        let (out, passthrough) = key_outcome(parent, wp);
        if !passthrough {
            dispatch(parent, out);
            return 0;
        }
        // passthrough：继续走下面的 EDIT 默认处理（输入字符、退格、左右移动光标）
    }

    // 回车/ESC 已被上面拦截；这里再挡一次 WM_CHAR，避免 EDIT 发出"叮"的提示音。
    if msg == WM_CHAR && (wp == VK_RETURN || wp == VK_ESCAPE) {
        return 0;
    }

    let old = (*ptr).edit_old_proc;
    if old == 0 {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    CallWindowProcW(
        std::mem::transmute::<isize, WNDPROC>(old),
        hwnd,
        msg,
        wp,
        lp,
    )
}

// --------------------------------------------------------------- 对外 API

pub fn show(hwnd: HWND, rows: Vec<ItemRow>, status: Option<String>) {
    if let Some(st) = unsafe { state(hwnd) } {
        st.set_rows(rows);
        st.status = status;
        st.query.clear();
        if !st.edit.is_null() {
            let empty = to_wide("");
            unsafe { SetWindowTextW(st.edit, empty.as_ptr()) };
        }
        st.place();
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            if !st.edit.is_null() {
                SetFocus(st.edit);
            }
        }
        invalidate(hwnd);
    }
}

pub fn hide(hwnd: HWND) {
    if !is_visible(hwnd) {
        return;
    }
    let owner = unsafe { state(hwnd).map(|st| st.owner) };
    unsafe { ShowWindow(hwnd, SW_HIDE) };
    if let Some(owner) = owner {
        unsafe { PostMessageW(owner, WM_POPUP_CLOSED, 0, 0) };
    }
}

pub fn is_visible(hwnd: HWND) -> bool {
    !hwnd.is_null() && unsafe { IsWindowVisible(hwnd) } != 0
}

/// 搜索后回填列表：尽量保留当前选中项，避免列表"跳一下"。
pub fn set_rows(hwnd: HWND, rows: Vec<ItemRow>) {
    if let Some(st) = unsafe { state(hwnd) } {
        let keep = st.current_id();
        st.set_rows(rows);
        if let Some(id) = keep {
            if let Some(idx) = st.rows.iter().position(|r| r.id == id) {
                st.nav.select(&st.metrics, idx);
            }
        }
        st.place();
        invalidate(hwnd);
    }
}

pub fn set_status(hwnd: HWND, status: Option<String>) {
    if let Some(st) = unsafe { state(hwnd) } {
        st.status = status;
        invalidate(hwnd);
    }
}

pub fn set_thumb(hwnd: HWND, id: i64, png: &[u8]) {
    if let Some(st) = unsafe { state(hwnd) } {
        if st.thumbs.get_or_insert(id, png).is_some() {
            invalidate(hwnd);
        }
    }
}

pub fn current_query(hwnd: HWND) -> String {
    unsafe { state(hwnd).map(|st| st.query.clone()).unwrap_or_default() }
}

pub fn selected_id(hwnd: HWND) -> Option<i64> {
    unsafe { state(hwnd).and_then(|st| st.current_id()) }
}

pub fn invalidate(hwnd: HWND) {
    unsafe {
        windows_sys::Win32::Graphics::Gdi::InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

/// 清除搜索框（用于每次弹出时重置）。
pub fn clear_query(hwnd: HWND) {
    if let Some(st) = unsafe { state(hwnd) } {
        if !st.edit.is_null() {
            let empty = to_wide("");
            unsafe { SetWindowTextW(st.edit, empty.as_ptr()) };
        }
        st.query.clear();
    }
}

/// DPI 变化后重建字体与度量。
pub fn rebuild_metrics(hwnd: HWND) {
    if let Some(st) = unsafe { state(hwnd) } {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        st.metrics = Metrics::new(if dpi == 0 { 96 } else { dpi });
        st.fonts = Fonts::create(&st.metrics);
        if !st.edit.is_null() {
            let font = st.fonts.ui;
            unsafe { SendMessageW(st.edit, WM_SETFONT, font as usize, 1) };
        }
        st.layout_edit();
        st.nav.clamp(&st.metrics);
        invalidate(hwnd);
    }
}

pub fn destroy(hwnd: HWND) {
    if !hwnd.is_null() {
        unsafe { DestroyWindow(hwnd) };
    }
}

// ----------------------------------------------------------------- 工具

fn read_edit_text(edit: HWND) -> String {
    if edit.is_null() {
        return String::new();
    }
    let len = unsafe { GetWindowTextLengthW(edit) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 2];
    let n = unsafe { GetWindowTextW(edit, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return String::new();
    }
    from_wide_until_nul(&buf[..n as usize])
}

/// `lParam` 高 16 位（鼠标 y 坐标，有符号）。
fn hiword(lp: LPARAM) -> i32 {
    ((lp as u32 >> 16) & 0xFFFF) as u16 as i16 as i32
}

fn is_down(vk: i32) -> bool {
    unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(vk) < 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::Kind;
    use crate::ui::theme::Metrics;

    fn m() -> Metrics {
        Metrics::new(96)
    }

    fn row(id: i64, kind: Kind, preview: &str) -> ItemRow {
        ItemRow {
            id,
            kind,
            preview: preview.to_string(),
            created_at: 0,
            last_used_at: 0,
            use_count: 0,
            pinned: false,
            source_exe: None,
            file_count: 0,
            img_w: 0,
            img_h: 0,
            thumb_state: 1,
        }
    }

    #[test]
    fn nav_clamps_and_keeps_selection_visible() {
        let m = m();
        let mut nav = ListNav::new(100);
        let vis = nav.visible(&m);
        assert!(vis >= 5);
        // 往下走超过一屏后，顶部要跟着滚
        for _ in 0..vis + 3 {
            nav.move_by(&m, 1);
        }
        assert_eq!(nav.selected, vis + 3);
        assert!(
            nav.top > 0,
            "选择跑到屏幕外了：top={} sel={}",
            nav.top,
            nav.selected
        );
        assert!(nav.selected < nav.top + vis);
        // 回到顶部
        nav.select(&m, 0);
        assert_eq!(nav.top, 0);
        // 末尾
        nav.select(&m, 99);
        assert_eq!(nav.selected, 99);
        assert!(nav.top + vis >= 100);
    }

    #[test]
    fn nav_handles_empty_list() {
        let m = m();
        let mut nav = ListNav::new(0);
        nav.move_by(&m, 5);
        nav.move_by(&m, -5);
        assert_eq!(nav.selected, 0);
        assert_eq!(nav.top, 0);
        assert_eq!(nav.range(&m), (0, 0));
        nav.clamp(&m);
        assert_eq!(nav.total, 0);
    }

    #[test]
    fn scroll_rows_keeps_selection_inside_viewport() {
        let m = m();
        let mut nav = ListNav::new(50);
        nav.select(&m, 20);
        nav.scroll_rows(&m, 10);
        let vis = nav.visible(&m);
        assert!(nav.selected >= nav.top && nav.selected < nav.top + vis);
        // 向上滚到顶
        nav.scroll_rows(&m, -1000);
        assert_eq!(nav.top, 0);
        // 向下越过底部
        nav.scroll_rows(&m, 1000);
        assert_eq!(nav.top, nav.max_top(&m));
    }

    #[test]
    fn hit_test_maps_y_to_row() {
        let m = m();
        assert_eq!(row_at(&m, 0, 0), None, "搜索框区域不算列表");
        assert_eq!(row_at(&m, 0, m.search_h), Some(0));
        assert_eq!(row_at(&m, 0, m.search_h + m.row_h - 1), Some(0));
        assert_eq!(row_at(&m, 0, m.search_h + m.row_h), Some(1));
        // 滚了 5 行后，点第一行应该是第 5 条
        assert_eq!(row_at(&m, 5, m.search_h + 1), Some(5));
        // 页脚区域不算
        let footer_y = m.search_h + m.list_height() + 1;
        assert_eq!(row_at(&m, 0, footer_y), None);
    }

    #[test]
    fn row_rect_is_contiguous() {
        let m = m();
        let r0 = row_rect(&m, 0, 0);
        let r1 = row_rect(&m, 0, 1);
        assert_eq!(r0.top, m.search_h);
        assert_eq!(r0.bottom, r1.top, "行之间不能有缝");
        assert_eq!(r1.bottom - r1.top, m.row_h);
    }

    #[test]
    fn key_actions() {
        let m = m();
        let mut nav = ListNav::new(10);
        assert_eq!(
            handle_key(&mut nav, &m, VK_ESCAPE, false, false).0,
            KeyAction::Close
        );
        assert_eq!(
            handle_key(&mut nav, &m, VK_RETURN, false, false).0,
            KeyAction::Choose {
                clipboard_only: false
            }
        );
        assert_eq!(
            handle_key(&mut nav, &m, VK_RETURN, false, true).0,
            KeyAction::Choose {
                clipboard_only: true
            }
        );
        assert_eq!(
            handle_key(&mut nav, &m, VK_DOWN, false, false).0,
            KeyAction::Consumed
        );
        assert_eq!(nav.selected, 1);
        assert_eq!(
            handle_key(&mut nav, &m, VK_END, false, false).0,
            KeyAction::Consumed
        );
        assert_eq!(nav.selected, 9);
        assert_eq!(
            handle_key(&mut nav, &m, VK_HOME, false, false).0,
            KeyAction::Consumed
        );
        assert_eq!(nav.selected, 0);
        // Ctrl+3 直选第 3 条
        assert_eq!(
            handle_key(&mut nav, &m, 0x33, true, false).0,
            KeyAction::PickIndex(2)
        );
        assert_eq!(nav.selected, 2);
        // Ctrl+9 超出总数 → 什么都不做
        let mut small = ListNav::new(3);
        assert_eq!(
            handle_key(&mut small, &m, 0x39, true, false).0,
            KeyAction::Consumed
        );
        assert_eq!(small.selected, 0);
        // Ctrl+P / Ctrl+D
        assert_eq!(
            handle_key(&mut nav, &m, 0x50, true, false).0,
            KeyAction::Pin
        );
        assert_eq!(
            handle_key(&mut nav, &m, 0x44, true, false).0,
            KeyAction::Delete
        );
        // 普通字母不拦截（要留给搜索框）
        assert_eq!(
            handle_key(&mut nav, &m, 0x41, false, false).0,
            KeyAction::None
        );
        assert_eq!(
            handle_key(&mut nav, &m, 0x08, false, false).0,
            KeyAction::None
        );
    }

    #[test]
    fn hiword_decodes_negative_coords() {
        let lp = ((-5i32 as u32 & 0xFFFF) << 16) as isize;
        assert_eq!(hiword(lp), -5);
    }

    #[test]
    fn row_model_is_usable_for_rendering_decisions() {
        let r = row(7, Kind::Image, "图片 100×50");
        assert_eq!(r.id, 7);
        assert!(r.kind == Kind::Image);
    }
}
