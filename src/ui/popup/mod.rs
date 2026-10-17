//! 弹窗/主窗口的**控制器**：状态、定位、窗口类与消息过程、结论派发、公开 API。
//!
//! 按职责拆成四个文件（原先 1825 行一个文件，命中测试与消息过程混在一起，
//! “设置按钮点不到”“右键窗口消失”这类 bug 都藏在这个夹缝里）：
//!
//! - [`nav`]：列表导航与命中测试 —— 不碰 HWND，纯逻辑可单测
//! - [`view`]：所有绘制
//! - [`chrome`]：标题栏按钮命中、圆角外框
//! - 本文件：状态结构、定位、消息过程、dispatch、公开 API
//!
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
    BeginPaint, EndPaint, GetMonitorInfoW, MonitorFromPoint, ScreenToClient, HDC, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, WHITE_BRUSH,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow,
    GetClientRect, GetCursorPos, GetForegroundWindow, GetParent, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    LoadCursorW, MoveWindow, PostMessageW, RegisterClassExW, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, CREATESTRUCTW, CS_DBLCLKS,
    CS_DROPSHADOW, CS_HREDRAW, CS_VREDRAW, ES_AUTOHSCROLL, ES_LEFT, HTCAPTION, HTCLIENT,
    HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_RESTORE, SW_SHOW,
    WM_ACTIVATE, WM_APP, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT, WM_DESTROY,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCCREATE, WM_NCHITTEST, WM_PAINT, WM_RBUTTONUP, WM_SETFONT, WM_SYSKEYDOWN, WNDCLASSEXW,
    WNDPROC, WS_CHILD, WS_CLIPCHILDREN, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::core::model::{ItemRow, Kind};
use crate::ui::gdi::{self, rect, Fonts, MemDc, ThumbCache};
use crate::ui::theme::{color, Metrics};
use crate::ui::timefmt;
use crate::win::strings::{from_wide_until_nul, to_wide};

mod chrome;
mod nav;
mod view;

pub use chrome::{title_hit, TitleHit};
pub use nav::{handle_key, row_at, row_rect, KeyAction, ListNav};

// 主窗口手动缩放用到的几个 Win32 符号（单独列出，免得和上面那串长导入互相干扰）
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, SWP_NOZORDER, WM_LBUTTONUP, WM_SIZE,
};

pub const CLASS_NAME: &str = "ClipHist.Popup";

/// 主窗口 → 控制器：请打开设置（标题栏上的"设置"按钮）。
pub const WM_LIST_OPEN_SETTINGS: u32 = WM_APP + 17;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Popup,
    Main,
}

impl Mode {
    fn metrics(self, dpi: u32) -> Metrics {
        match self {
            Mode::Popup => Metrics::new(dpi),
            Mode::Main => Metrics::main_window(dpi),
        }
    }
}

/// 用静态地址当"主窗口模式"的标记：`CreateWindowExW` 的 lpCreateParams 只有一格，
/// 传非法指针是不干净的做法，指一个静态字节则完全合法。
static MAIN_MODE_TAG: u8 = 1;
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
/// 列表 → 控制器：把这条加入/移出粘贴队列（wParam = 发送者）。
pub const WM_POPUP_QUEUE: u32 = WM_APP + 22;
/// 列表 → 控制器：把队列合并成一段多行文本复制走（wParam = 发送者）。
pub const WM_POPUP_MERGE: u32 = WM_APP + 23;
/// 列表 → 控制器：用系统看图程序打开这条图片。
pub const WM_POPUP_VIEW_IMAGE: u32 = WM_APP + 24;

const EN_CHANGE: usize = 0x0300;
const VK_RETURN: usize = 0x0D;
const VK_ESCAPE: usize = 0x1B;
const VK_SPACE: usize = 0x20;
/// 主窗口拖拽缩放的最小尺寸
const MIN_W: i32 = 520;
const MIN_H: i32 = 380;
const VK_UP: usize = 0x26;
const VK_DOWN: usize = 0x28;
const VK_PRIOR: usize = 0x21;
const VK_NEXT: usize = 0x22;
const VK_HOME: usize = 0x24;
const VK_END: usize = 0x23;
const VK_CONTROL: i32 = 0x11;
const VK_SHIFT: i32 = 0x10;

/// 弹窗状态。生命周期与窗口一致。
struct PopupState {
    hwnd: HWND,
    owner: HWND,
    edit: HWND,
    edit_old_proc: isize,
    fonts: Fonts,
    metrics: Metrics,
    mode: Mode,
    mem: Option<MemDc>,
    nav: ListNav,
    rows: Vec<ItemRow>,
    hover: Option<usize>,
    thumbs: ThumbCache,
    /// 已经向控制器请求过缩略图的 id，避免每帧重复请求。
    requested_thumbs: HashSet<i64>,
    status: Option<String>,
    query: String,
    /// DWM 是否给了圆角（Win11）。给了就画圆角边框，否则画直角边框，
    /// 免得那条 1px 边框从圆角窗口的四个角里探出来。
    rounded: bool,
    /// 呼出时定下的锚点：(x, 固定边 y, 是否朝上翻)。之后行数变化只改高度，
    /// 位置纹丝不动 —— 打字时弹窗会跑，比丑难受得多。
    anchor: Option<(i32, i32, bool)>,
    /// 右键菜单打开期间置位：此时窗口失去激活是"菜单拿走了焦点"，
    /// 不是"用户点了别处"，绝不能因此关窗。
    menu_open: bool,
    /// 粘贴队列（有序的条目 id），只存在内存里 —— 它是一次性动作，不该进库。
    queue: Vec<i64>,
    /// 正在拖拽缩放：边标志(1左 2右 4上 8下)、按下时的屏幕坐标与窗口尺寸。
    resizing: Option<(u32, i32, i32, i32, i32)>,
    /// 仅预览模式用：离屏渲染不包含子窗口（EDIT），所以由父窗口代画输入框内容
    /// （空字符串表示"空输入框"，会画一根光标），这样预览图才跟用户看到的一致。
    preview_text: Option<String>,
}

/// 我们自己设置搜索框文本期间，忽略随之而来的 `EN_CHANGE`。
///
/// 用**静态变量**而不是 `PopupState` 字段，是因为 `SetWindowTextW` 会同步回调父窗口过程：
/// 那时若再去取一次 `&mut PopupState` 就是别名借用（和 `ShowWindow` 那个坑同一类）。
static SUPPRESS_EN_CHANGE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

impl PopupState {
    fn new(hwnd: HWND, owner: HWND, mode: Mode) -> PopupState {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let metrics = mode.metrics(if dpi == 0 { 96 } else { dpi });
        PopupState {
            hwnd,
            owner,
            edit: std::ptr::null_mut(),
            edit_old_proc: 0,
            fonts: Fonts::create(&metrics),
            metrics,
            mode,
            mem: None,
            nav: ListNav::new(0),
            rows: Vec::new(),
            hover: None,
            thumbs: ThumbCache::new(24),
            requested_thumbs: HashSet::new(),
            status: None,
            query: String::new(),
            rounded: false,
            menu_open: false,
            queue: Vec::new(),
            resizing: None,
            anchor: None,
            preview_text: None,
        }
    }

    /// 设置搜索框文本，并抑制由此同步触发的 `EN_CHANGE`。
    fn set_edit_text(&self, text: &str) {
        if self.edit.is_null() {
            return;
        }
        use std::sync::atomic::Ordering;
        SUPPRESS_EN_CHANGE.store(true, Ordering::SeqCst);
        let wide = to_wide(text);
        unsafe { SetWindowTextW(self.edit, wide.as_ptr()) };
        SUPPRESS_EN_CHANGE.store(false, Ordering::SeqCst);
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
                // hMenu 就是子控件的 ID。这里曾经传 null（ID=0），于是 EDIT 发来的
                // EN_CHANGE 里 id 恒为 0，而 WM_COMMAND 里判断的是 EDIT_ID —— 每条
                // 通知都被丢掉，表现就是"搜索框能打字但列表不动"。
                EDIT_ID as isize as *mut core::ffi::c_void,
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

    /// 搜索框位置：紧跟在"搜索"标签之后，横跨到右边距。
    ///
    /// 输入框刻意**不带边框、不带底色**：整个顶部只有一行文字和它下面的一条实线，
    /// 看起来像在纸上写的一行字，而不是一个灰色输入框（后者是"控制面板"的味道）。
    fn layout_edit(&self) {
        if self.edit.is_null() {
            return;
        }
        let m = &self.metrics;
        let left = m.pad + m.mark + 4;
        // 与 render_header 用同一套算法（同样的字体度量），基线才对得上。
        let hdc = unsafe { windows_sys::Win32::Graphics::Gdi::GetDC(self.hwnd) };
        let h = gdi::text_height(hdc, self.fonts.ui).max(m.fs_preview);
        unsafe { windows_sys::Win32::Graphics::Gdi::ReleaseDC(self.hwnd, hdc) };
        let top = m.title_h + (m.header_h - h) / 2;
        unsafe {
            MoveWindow(
                self.edit,
                left,
                top,
                (m.width - m.pad - left).max(10),
                h + 2,
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
        if self.mode == Mode::Main {
            // 主窗口：屏幕**绝对居中**、固定大小。这是它和浮动列表最大的区别 ——
            // 浮动列表跟着光标走（要快），主窗口固定在视觉中心（要稳）。
            let (x, y) = crate::win::screen::center_on_primary(m.width, m.max_height);
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    x,
                    y,
                    m.width,
                    m.max_height,
                    SWP_SHOWWINDOW,
                )
            };
            return;
        }
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
        let mut flipped = false;
        let mut y = pt.y + gap;
        if y + h > work.bottom - gap {
            // 下方放不下就翻到光标上方
            y = pt.y - h - gap;
            flipped = true;
        }
        y = y.clamp(work.top + gap, (work.bottom - h - gap).max(work.top + gap));

        // 记住这次定下的位置：朝上翻时固定底边，否则固定顶边。
        self.anchor = Some((x, if flipped { y + h } else { y }, flipped));
        unsafe { SetWindowPos(self.hwnd, HWND_TOPMOST, x, y, m.width, h, SWP_SHOWWINDOW) };
    }

    /// 内容变化（搜索过滤）时**只改高度、不动位置**。
    ///
    /// 早先每次过滤都重新 `place()`：重新读一次光标、重新决定朝上还是朝下，
    /// 结果就是打字时弹窗在屏幕上跳来跳去。现在锚点在呼出时定一次，之后只升降高度。
    fn keep_anchor(&mut self) {
        let m = self.metrics;
        if let Some((x, edge_y, flipped)) = self.anchor {
            let h = m.height_for(self.rows.len());
            let y = if flipped { edge_y - h } else { edge_y };
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    x,
                    y,
                    m.width,
                    h,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )
            };
            return;
        }
        // 第一次（还没定过位）就正常定位。
        self.place();
    }

    // ---------------------------------------------------------- 绘制
}

/// 文件夹轮廓：形状比文字标签更快被认出来，而且不占版面。
fn draw_folder(dc: HDC, box_rc: &RECT, color: windows_sys::Win32::Foundation::COLORREF) {
    let w = box_rc.right - box_rc.left;
    let h = box_rc.bottom - box_rc.top;
    let left = box_rc.left + w / 8;
    let right = box_rc.right - w / 8;
    let tab_top = box_rc.top + h / 6;
    let tab_bottom = tab_top + (h / 6).max(3);
    let bottom = box_rc.bottom - h / 6;
    // 左上角的标签突起
    gdi::frame(
        dc,
        &rect(left, tab_top, left + (w * 5) / 12, tab_bottom + 1),
        color,
    );
    // 主体
    gdi::frame(dc, &rect(left, tab_bottom, right, bottom), color);
}

// ------------------------------------------------------------- 窗口过程

pub fn register_class(hinstance: HINSTANCE) -> bool {
    let cls = to_wide(CLASS_NAME);
    let mut wc: WNDCLASSEXW = unsafe { zeroed() };
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.style = CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW | CS_DBLCLKS;
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
pub fn create(owner: HWND, hinstance: HINSTANCE, mode: Mode) -> Option<HWND> {
    let cls = to_wide(CLASS_NAME);
    let title = to_wide(crate::APP_TITLE);
    let params = match mode {
        Mode::Popup => std::ptr::null(),
        Mode::Main => &MAIN_MODE_TAG as *const u8 as *const core::ffi::c_void,
    };
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
            params,
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
    Queue { id: i64 },
    MergeQueue,
    ViewImage,
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
        Outcome::ViewImage => {
            if let Some(st) = state(hwnd) {
                if let Some(id) = st.current_id() {
                    PostMessageW(owner, WM_POPUP_VIEW_IMAGE, 0, id as isize);
                }
            }
        }
        Outcome::MergeQueue => {
            if !owner.is_null() {
                PostMessageW(owner, WM_POPUP_MERGE, hwnd as usize, 0);
            }
        }
        Outcome::Queue { id } => {
            if !owner.is_null() {
                PostMessageW(owner, WM_POPUP_QUEUE, hwnd as usize, id as isize);
            }
        }
        Outcome::Context { id } => {
            if !owner.is_null() {
                // wParam 带上自己：控制器才知道该给哪个窗口加"菜单保护"。
                PostMessageW(owner, WM_POPUP_CONTEXT, hwnd as usize, id as isize);
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
        KeyAction::MergeQueue => Outcome::MergeQueue,
        KeyAction::ViewImage => Outcome::ViewImage,
        KeyAction::Queue => match id {
            Some(id) => Outcome::Queue { id },
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

/// 鼠标点在某一行上：选中它；`activate` 为真时再"选择并粘贴"。
///
/// 单击/双击共用这一段，差异只有 `activate` 一个参数 ——
/// 免得两处命中测试各写一遍、日后改一处忘一处。
unsafe fn activate_row(hwnd: HWND, y: i32, activate: bool) -> Outcome {
    let Some(st) = state(hwnd) else {
        return Outcome::None;
    };
    match row_at(&st.metrics, st.nav.top, y) {
        Some(idx) if idx < st.rows.len() => {
            st.nav.select(&st.metrics, idx);
            st.hover = Some(idx);
            if activate {
                let id = st.rows[idx].id;
                Outcome::Choose {
                    id,
                    clipboard_only: is_down(VK_SHIFT),
                }
            } else {
                Outcome::Redraw
            }
        }
        _ => Outcome::None,
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
            // lpCreateParams 非空即表示"主窗口模式"（见 MAIN_MODE_TAG）。
            let mode = if cs.is_null() || (*cs).lpCreateParams.is_null() {
                Mode::Popup
            } else {
                Mode::Main
            };
            let state = Box::new(PopupState::new(hwnd, owner, mode));
            SetWindowLongPtrW(
                hwnd,
                windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
                Box::into_raw(state) as isize,
            );
            return 1; // 继续创建
        }
        WM_CREATE => {
            if let Some(st) = state(hwnd) {
                st.rounded = chrome::apply_rounded_corners(hwnd);
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
                drop(Box::from_raw(p));
            }
            return 0;
        }
        WM_SIZE => {
            // 主窗口可缩放：把新的客户区尺寸写回度量，排版随之重算。
            if let Some(st) = state(hwnd) {
                if st.mode == Mode::Main {
                    let (w, h) = (loword(lp), hiword(lp));
                    if w > 0 && h > 0 {
                        st.metrics.width = w;
                        st.metrics.max_height = h;
                        st.layout_edit();
                        invalidate(hwnd);
                    }
                }
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
            // 输入框与纸面同色：顶部只有一行字和一条下划线，没有"输入框"这个物体。
            windows_sys::Win32::Graphics::Gdi::SetTextColor(wp as HDC, color::INK);
            windows_sys::Win32::Graphics::Gdi::SetBkColor(wp as HDC, color::PAPER);
            return unsafe { windows_sys::Win32::Graphics::Gdi::GetStockObject(WHITE_BRUSH) }
                as isize;
        }
        WM_COMMAND => {
            let id = wp & 0xFFFF;
            let code = (wp >> 16) & 0xFFFF;
            if id != EDIT_ID || code != EN_CHANGE {
                return 0;
            }
            use std::sync::atomic::Ordering;
            // 我们自己在改文本（呼出时清空搜索框）触发的通知要忽略。
            // 这个标记是静态的，所以不用担心在 &mut 借用期间回读状态。
            if SUPPRESS_EN_CHANGE.load(Ordering::SeqCst) {
                return 0;
            }
            if let Some(st) = state(hwnd) {
                let text = read_edit_text(st.edit);
                if text != st.query {
                    st.query = text;
                    // wParam 带上自己的窗口句柄：浮动列表和主窗口共用一个控制器，
                    // 不说是谁在打字，控制器就只能去更新固定那一个（真 bug）。
                    PostMessageW(st.owner, WM_POPUP_QUERY, hwnd as usize, 0);
                }
            }
            return 0;
        }
        WM_ACTIVATE => {
            // 点别处 / 切走即关闭 —— 这是**浮动列表**的手感（Win+V 那样）。
            //
            // 主窗口不能照抄：右键菜单、Alt+Tab、点一下别的窗口都会让它短暂失活，
            // 若也跟着关掉，用户一在主窗口里右键，整个窗口就没了（真 bug）。
            // 主窗口只在按 ✕ / Esc 时关闭。
            let (popup_mode, menu_open) = state(hwnd)
                .map(|st| (st.mode == Mode::Popup, st.menu_open))
                .unwrap_or((false, false));
            // 菜单打开时失去激活是"菜单拿走了焦点"，不是用户点了别处 —— 不能关窗。
            if (wp & 0xFFFF) as u32 == 0 && popup_mode && !menu_open && is_visible(hwnd) {
                dispatch(hwnd, Outcome::Close);
            }
            return 0;
        }
        WM_MOUSEMOVE => {
            // 缩放中：按屏幕光标位移算新尺寸（左/上边拖动时原点也要动）。
            let scaling = state(hwnd).and_then(|st| st.resizing);
            if let Some((bits, _, _, w0, h0)) = scaling {
                let mut pt = POINT { x: 0, y: 0 };
                unsafe { GetCursorPos(&mut pt) };
                let anchor = state(hwnd).map(|_st| {
                    let mut rc = rect(0, 0, 0, 0);
                    unsafe { GetWindowRect(hwnd, &mut rc) };
                    (rc.left, rc.top)
                });
                if let Some((left0, top0)) = anchor {
                    let mut x = left0;
                    let mut y = top0;
                    let mut w = w0;
                    let mut h = h0;
                    if bits & 1 != 0 {
                        let dx = left0 - pt.x;
                        w += dx;
                        x = pt.x;
                    }
                    if bits & 2 != 0 {
                        w += pt.x - (left0 + w0);
                    }
                    if bits & 4 != 0 {
                        let dy = top0 - pt.y;
                        h += dy;
                        y = pt.y;
                    }
                    if bits & 8 != 0 {
                        h += pt.y - (top0 + h0);
                    }
                    let (w, h) = (w.max(MIN_W), h.max(MIN_H));
                    unsafe {
                        SetWindowPos(
                            hwnd,
                            std::ptr::null_mut(),
                            x,
                            y,
                            w,
                            h,
                            SWP_NOZORDER | 0x0010,
                        )
                    };
                    if let Some(st) = state(hwnd) {
                        st.resizing = Some((bits, 0, 0, w, h));
                    }
                }
                return 0;
            }
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
        WM_NCHITTEST => {
            // 主窗口没有系统标题栏，拖动必须保留；但**只有标题栏的空白部分**返回
            // HTCAPTION —— 右侧"设置 / ✕"必须留给客户区，否则点击会被当成拖动吃掉。
            let m = state(hwnd).map(|st| st.metrics);
            if let Some(m) = m {
                if m.title_h > 0 {
                    let mut pt = POINT {
                        x: loword(lp),
                        y: hiword(lp),
                    };
                    unsafe { ScreenToClient(hwnd, &mut pt) };
                    if title_hit(&m, pt.x, pt.y) == TitleHit::Drag {
                        return HTCAPTION as isize;
                    }
                }
            }
            return HTCLIENT as isize;
        }
        WM_LBUTTONDOWN => {
            // 主窗口边缘 8px 内按下 → 进入手动缩放。
            // 不挂 WS_THICKFRAME：那会让系统额外画一圈边框，破坏自绘外观。
            if let Some((cx, cy, w, h)) = state(hwnd).map(|_st| {
                let mut rc = rect(0, 0, 0, 0);
                unsafe { GetClientRect(hwnd, &mut rc) };
                (loword(lp), hiword(lp), rc.right, rc.bottom)
            }) {
                const EDGE: i32 = 8;
                let mut bits = 0u32;
                if cx <= EDGE {
                    bits |= 1
                }
                if cx >= w - EDGE {
                    bits |= 2
                }
                if cy <= EDGE {
                    bits |= 4
                }
                if cy >= h - EDGE {
                    bits |= 8
                }
                let is_main = state(hwnd).map(|st| st.mode == Mode::Main).unwrap_or(false);
                if is_main && bits != 0 {
                    if let Some(st) = state(hwnd) {
                        st.resizing = Some((bits, 0, 0, w, h));
                        unsafe { SetCapture(hwnd) };
                    }
                    return 0;
                }
            }

            // 主窗口的标题栏按钮：先算命中（拿到纯值），再动作 —— 因为
            // hide() 会同步派发消息，动作期间不能还握着 &mut PopupState。
            let title_click =
                state(hwnd).map(|st| (title_hit(&st.metrics, loword(lp), hiword(lp)), st.owner));
            if let Some((hit, owner)) = title_click {
                match hit {
                    TitleHit::Close => {
                        hide(hwnd);
                        return 0;
                    }
                    TitleHit::Settings => {
                        if !owner.is_null() {
                            unsafe { PostMessageW(owner, WM_LIST_OPEN_SETTINGS, 0, 0) };
                        }
                        return 0;
                    }
                    TitleHit::Drag | TitleHit::None => {}
                }
            }

            // 命中行的语义按窗口身份分开：
            // - 浮动列表：单击即粘贴（与 Win+V 一致，快是唯一目的）
            // - 主窗口：单击只选中，双击/回车才粘贴（用户可能只是在翻着看）
            // 浮动列表：单击即粘贴；主窗口：单击只选中（双击才粘贴）。
            let activate = state(hwnd)
                .map(|st| st.mode == Mode::Popup)
                .unwrap_or(false);
            let out = activate_row(hwnd, hiword(lp), activate);
            dispatch(hwnd, out);
            return 0;
        }
        WM_LBUTTONUP => {
            let scaling = state(hwnd).and_then(|st| st.resizing);
            if scaling.is_some() {
                if let Some(st) = state(hwnd) {
                    st.resizing = None;
                }
                unsafe { ReleaseCapture() };
                return 0;
            }
            return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
        }
        WM_LBUTTONDBLCLK => {
            // 只有主窗口会走到这里（浮动列表单击就已经粘贴并关窗了）。
            let out = activate_row(hwnd, hiword(lp), true);
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
        st.anchor = None; // 每次呼出都重新按光标定一次位
        st.set_edit_text("");
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

/// 把窗口提到最前并尝试取得前台焦点。
///
/// 托盘点击后 `SetForegroundWindow` 经常被 Windows 的"前台锁定"拒绝，结果窗口虽然
/// ShowWindow 了却压在别的窗口后面 —— 用户看到的就是"点了没反应，再点一下才有"。
/// 这里用 AttachThreadInput 借用前台线程的输入队列，是托盘类程序的标准做法。
pub fn bring_to_front(hwnd: HWND) {
    if hwnd.is_null() {
        return;
    }
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        let tid_cur = GetCurrentThreadId();
        let tid_fg = GetWindowThreadProcessId(fg, std::ptr::null_mut());
        let attached = !fg.is_null() && tid_fg != 0 && tid_fg != tid_cur && {
            AttachThreadInput(tid_cur, tid_fg, 1) != 0
        };
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        if attached {
            AttachThreadInput(tid_cur, tid_fg, 0);
        }
    }
}

/// 当前前台窗口是不是它。
pub fn is_foreground(hwnd: HWND) -> bool {
    !hwnd.is_null() && unsafe { GetForegroundWindow() } == hwnd
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
        st.keep_anchor();
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

/// 更新粘贴队列（有序条目 id）。控制器在队列变化后调用。
pub fn set_queue(hwnd: HWND, ids: Vec<i64>) {
    if let Some(st) = unsafe { state(hwnd) } {
        st.queue = ids;
        invalidate(hwnd);
    }
}

/// 标记/清除"右键菜单打开中"。
pub fn set_menu_open(hwnd: HWND, open: bool) {
    if let Some(st) = unsafe { state(hwnd) } {
        st.menu_open = open;
    }
}

/// 清除搜索框（用于每次弹出时重置）。
pub fn clear_query(hwnd: HWND) {
    if let Some(st) = unsafe { state(hwnd) } {
        st.query.clear();
        st.anchor = None; // 每次呼出都重新按光标定一次位
        st.set_edit_text("");
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

/// 开发者预览：把弹窗内容渲染成一张 RGBA 图（**不显示窗口**，也不需要剪贴板）。
///
/// 存在的理由：改完视觉总得能看见结果。这里渲染走的是正式绘制路径（同一个
/// [`PopupState::render`]），所以截出来的就是用户会看到的画面 ——
/// 而不是"另写一套预览代码"那种自欺的假截图。
pub fn render_preview(
    hwnd: HWND,
    rows: Vec<ItemRow>,
    status: Option<String>,
    query: &str,
    thumbs: &[(i64, Vec<u8>)],
    w: i32,
    h: i32,
) -> Option<Vec<u8>> {
    let st = unsafe { state(hwnd) }?;
    for (id, png) in thumbs {
        st.thumbs.get_or_insert(*id, png);
    }
    st.set_rows(rows);
    st.status = status;
    st.query = query.to_string();
    st.preview_text = Some(query.to_string());
    // 选中第 2 行、悬停第 3 行：一张图里就能同时看到反相选中与 hover 两种状态。
    st.nav.select(&st.metrics, 1);
    st.hover = Some(2);

    let mut mem = MemDc::new(w, h)?;
    st.render(mem.dc(), w, h);
    mem.to_rgba()
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
fn loword(lp: LPARAM) -> i32 {
    (lp as u32 & 0xFFFF) as u16 as i16 as i32
}

fn hiword(lp: LPARAM) -> i32 {
    ((lp as u32 >> 16) & 0xFFFF) as u16 as i16 as i32
}

fn is_down(vk: i32) -> bool {
    unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(vk) < 0 }
}

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn title_bar_hit_areas_are_disjoint() {
        let m = Metrics::main_window(96);
        // 左侧空白 = 拖动区
        assert_eq!(title_hit(&m, 0, 5), TitleHit::Drag);
        assert_eq!(title_hit(&m, m.width / 2, 5), TitleHit::Drag);
        // 右起第一格 = 关闭，第二格 = 设置（顺序与绘制一致）
        assert_eq!(title_hit(&m, m.width - 1, 5), TitleHit::Close);
        assert_eq!(title_hit(&m, m.width - m.title_h, 5), TitleHit::Close);
        assert_eq!(
            title_hit(&m, m.width - m.title_h - 1, 5),
            TitleHit::Settings
        );
        assert_eq!(
            title_hit(&m, m.width - 2 * m.title_h, 5),
            TitleHit::Settings
        );
        // 边界：设置区左边一格必须回到拖动区（按钮不能吞掉整条标题栏）
        assert_eq!(
            title_hit(&m, m.width - 2 * m.title_h - 1, 5),
            TitleHit::Drag
        );
        // 标题栏以下不是标题栏
        assert_eq!(title_hit(&m, 100, m.title_h), TitleHit::None);
        // 浮动列表根本没有标题栏
        let p = Metrics::new(96);
        assert_eq!(title_hit(&p, 10, 5), TitleHit::None);
    }
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
        assert_eq!(row_at(&m, 0, m.header_h), Some(0));
        assert_eq!(row_at(&m, 0, m.header_h + m.row_h - 1), Some(0));
        assert_eq!(row_at(&m, 0, m.header_h + m.row_h), Some(1));
        // 滚了 5 行后，点第一行应该是第 5 条
        assert_eq!(row_at(&m, 5, m.header_h + 1), Some(5));
        // 页脚区域不算
        let footer_y = m.header_h + m.list_height() + 1;
        assert_eq!(row_at(&m, 0, footer_y), None);
    }

    #[test]
    fn row_rect_is_contiguous() {
        let m = m();
        let r0 = row_rect(&m, 0, 0);
        let r1 = row_rect(&m, 0, 1);
        assert_eq!(r0.top, m.header_h);
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
