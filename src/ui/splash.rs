//! 启动动画：一张卡片落定，然后自己消失。
//!
//! 设计意图：这个程序没有"主窗口级别的首页"，启动时能被感知的只有托盘图标出现的那一下。
//! 所以开场不做炫技，只做一件和产品有关的事 —— 一张纸卡片从下方 10px 落到位，
//! 底部一条墨线扫过（表示"正在挂上剪贴板监听"），然后整张淡出。
//!
//! 三条硬约束：
//! - **不抢焦点**：`WS_EX_NOACTIVATE` + `SW_SHOWNOACTIVATE`。用户正在打字时程序自启，
//!   焦点绝不能跳走。
//! - **不阻塞**：动画由 `SetTimer` 推进，和主消息循环共用一条线程，初始化照常继续。
//! - **不占内存**：整体透明度走 `SetLayeredWindowAttributes`（不做逐像素 alpha，因此不用
//!   维护 DIB）；圆角用窗口区域裁出来；800ms 后自己 `DestroyWindow`，字体一并释放。

use std::mem::zeroed;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, InvalidateRect, SetWindowRgn, HDC, PAINTSTRUCT,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, KillTimer, LoadCursorW,
    RegisterClassExW, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, CREATESTRUCTW, CS_DROPSHADOW, IDC_ARROW, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    SW_SHOWNOACTIVATE, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_NCCREATE, WM_PAINT, WM_TIMER,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::ui::gdi::{self, rect, Fonts, MemDc};
use crate::ui::theme::{color, Metrics};
use crate::win::strings::to_wide;

const CLASS_NAME: &str = "ClipHist.Splash";

/// 动画播完 → 控制器（该把主窗口拿出来了）。
pub const WM_SPLASH_DONE: u32 = 0x8000 + 18;
/// 窗口尺寸（逻辑像素，随 DPI 缩放）。
const W: i32 = 340;
const H: i32 = 132;
const DURATION_MS: i64 = 820;
const FADE_IN_MS: i64 = 170;
/// 从这一刻开始淡出。
const FADE_OUT_AT_MS: i64 = 640;
const SLIDE_PX: i32 = 10;
/// `LWA_ALPHA`
const LWA_ALPHA: u32 = 0x0000_0002;
/// `GWLP_USERDATA`
const GWLP_USERDATA: i32 = -21;

/// 动画进度 → 不透明度（0..255）。纯函数，便于单元测试。
pub fn alpha_at(elapsed_ms: i64) -> u8 {
    if elapsed_ms <= 0 {
        return 0;
    }
    if elapsed_ms < FADE_IN_MS {
        // 淡入用 sqrt：起步快、收尾稳，看起来像"灯亮起来"而不是线性爬升。
        let e = elapsed_ms as f64 / FADE_IN_MS as f64;
        return (255.0 * e.sqrt()).round() as u8;
    }
    if elapsed_ms >= DURATION_MS {
        return 0;
    }
    if elapsed_ms < FADE_OUT_AT_MS {
        return 255;
    }
    let e = (elapsed_ms - FADE_OUT_AT_MS) as f64 / (DURATION_MS - FADE_OUT_AT_MS) as f64;
    (255.0 * (1.0 - e)).round().clamp(0.0, 255.0) as u8
}

/// 底部墨线的扫过进度（0..1）。
pub fn progress_at(elapsed_ms: i64) -> f32 {
    let span = (FADE_OUT_AT_MS - FADE_IN_MS) as f64;
    (((elapsed_ms - FADE_IN_MS) as f64) / span).clamp(0.0, 1.0) as f32
}

/// 落定过程中额外下移的像素（0 表示已到位）。
pub fn slide_at(elapsed_ms: i64) -> i32 {
    if elapsed_ms >= FADE_IN_MS {
        return 0;
    }
    let e = (elapsed_ms as f64 / FADE_IN_MS as f64).clamp(0.0, 1.0);
    // 缓出：开始掉得快，收尾轻轻贴合。
    (SLIDE_PX as f64 * (1.0 - e * e)) as i32
}

/// 动画是否结束（该销毁窗口了）。
pub fn is_finished(elapsed_ms: i64) -> bool {
    elapsed_ms >= DURATION_MS
}

struct Splash {
    hwnd: HWND,
    /// 动画结束时要通知的窗口（控制器）。
    owner: HWND,
    fonts: Fonts,
    metrics: Metrics,
    started_at: std::time::Instant,
    base_x: i32,
    base_y: i32,
    w: i32,
    h: i32,
    mem: Option<MemDc>,
}

impl Splash {
    fn elapsed(&self) -> i64 {
        self.started_at.elapsed().as_millis() as i64
    }

    fn advance(&mut self) -> bool {
        let e = self.elapsed();
        if is_finished(e) {
            return false;
        }
        unsafe {
            SetLayeredWindowAttributes(self.hwnd, 0, alpha_at(e), LWA_ALPHA);
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                self.base_x,
                self.base_y + slide_at(e),
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
            );
            InvalidateRect(self.hwnd, std::ptr::null(), 0);
        }
        true
    }

    fn paint(&mut self, target: HDC) {
        let (w, h) = (self.w, self.h);
        if w <= 0 || h <= 0 {
            return;
        }
        let need_new = match &self.mem {
            Some(m) => m.size() != (w, h),
            None => true,
        };
        if need_new {
            self.mem = MemDc::new(w, h);
        }
        let Some(mem) = self.mem.take() else { return };
        self.draw(mem.dc(), w, h);
        mem.blit_to(target);
        self.mem = Some(mem);
    }

    fn draw(&self, dc: HDC, w: i32, h: i32) {
        let m = &self.metrics;
        gdi::fill(dc, &rect(0, 0, w, h), color::PAPER);
        let radius = (m.dpi as i32 / 10).max(8);
        gdi::round_frame(dc, &rect(0, 0, w - 1, h - 1), color::RULE_STRONG, radius);

        // 左侧标记：一张卡片 + 顶上的夹子 + 两条内容线（和托盘图标同一套语汇）
        let pad = m.pad;
        let mark = 34;
        let top = (h - mark) / 2 - 6;
        let mark_rc = rect(pad, top, pad + mark, top + mark);
        gdi::round_frame(dc, &mark_rc, color::INK, (m.dpi as i32) / 32);
        let clip_w = mark / 3;
        let clip = rect(
            mark_rc.left + (mark - clip_w) / 2,
            mark_rc.top - 3,
            mark_rc.left + (mark + clip_w) / 2,
            mark_rc.top + 5,
        );
        gdi::fill(dc, &clip, color::INK);
        for (i, frac) in [38, 58].iter().enumerate() {
            let y = mark_rc.top + mark * frac / 100 - 1;
            let inset = 7 + i as i32 * 2;
            gdi::hline(
                dc,
                mark_rc.left + inset,
                mark_rc.right - inset,
                y,
                color::RULE_STRONG,
            );
        }

        // 文字：标题 + 一行等宽提示（等宽那行说的正是"现在正在做什么"）
        let text_left = mark_rc.right + m.gap + 4;
        // 文字块整体在竖直方向居中（不跟着标记上移，那样会显得上半部发飘）
        let title_h = m.fs_preview + 8;
        let sub_h = m.fs_mono + 10;
        let text_top = (h - (title_h + sub_h)) / 2 - 2;
        let title_rc = rect(text_left, text_top, w - pad, text_top + title_h);
        gdi::text(
            dc,
            self.fonts.ui,
            color::INK,
            &title_rc,
            "剪贴板历史",
            false,
        );
        let sub_rc = rect(text_left, title_rc.bottom, w - pad, title_rc.bottom + sub_h);
        gdi::text(
            dc,
            self.fonts.mono,
            color::INK3,
            &sub_rc,
            "正在挂上剪贴板监听",
            false,
        );

        // 底部墨线：一条浅色轨道 + 随进度生长的墨段
        let line_y = h - pad - 2;
        let (x0, x1) = (pad, w - pad);
        gdi::hline(dc, x0, x1, line_y, color::RULE);
        let grew = x0 + ((x1 - x0) as f32 * progress_at(self.elapsed())) as i32;
        if grew > x0 {
            gdi::fill(dc, &rect(x0, line_y - 1, grew, line_y + 1), color::INK);
        }
    }
}

fn state(hwnd: HWND) -> Option<&'static mut Splash> {
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Splash).as_mut() }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = lp as *const CREATESTRUCTW;
            let created = unsafe { (*cs).lpCreateParams as *mut Splash };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, created as isize) };
            unsafe { (*created).hwnd = hwnd };
            return 1;
        }
        WM_CREATE => {
            if let Some(st) = state(hwnd) {
                let dpi = crate::win::dpi::dpi_of_window(hwnd);
                let metrics = Metrics::new(if dpi == 0 { 96 } else { dpi });
                st.metrics = metrics;
                st.fonts = Fonts::create(&metrics);
                let (w, h) = window_size(if dpi == 0 { 96 } else { dpi });
                st.w = w;
                st.h = h;
                // 圆角用窗口区域裁：分层窗口只支持整体透明度，做不了逐像素 alpha，
                // 否则四个角会露出方形的白边。
                let r = (metrics.dpi as i32 / 10).max(8);
                let rgn = unsafe { CreateRoundRectRgn(0, 0, w + 1, h + 1, r * 2, r * 2) };
                if !rgn.is_null() {
                    unsafe { SetWindowRgn(hwnd, rgn, 1) };
                }
                unsafe { SetTimer(hwnd, 1, 15, None) };
            }
            return 0;
        }
        WM_TIMER => {
            if let Some(st) = state(hwnd) {
                if !st.advance() {
                    let owner = st.owner;
                    unsafe { KillTimer(hwnd, 1) };
                    unsafe { DestroyWindow(hwnd) };
                    // 动画放完再收尾：告诉控制器"可以进主界面了"。
                    if !owner.is_null() {
                        unsafe {
                            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                                owner,
                                WM_SPLASH_DONE,
                                0,
                                0,
                            )
                        };
                    }
                }
            }
            return 0;
        }
        WM_PAINT => {
            if let Some(st) = state(hwnd) {
                let mut ps: PAINTSTRUCT = unsafe { zeroed() };
                let dc = unsafe { BeginPaint(hwnd, &mut ps) };
                st.paint(dc);
                unsafe { EndPaint(hwnd, &ps) };
                return 0;
            }
        }
        WM_ERASEBKGND => return 1,
        WM_DESTROY => {
            let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Splash;
            if !p.is_null() {
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                // 这里的 Box 由 show() 创建、由窗口负责销毁。
                drop(unsafe { Box::from_raw(p) });
            }
            return 0;
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// 注册窗口类。
pub fn register_class(hinstance: HINSTANCE) -> bool {
    let cls = to_wide(CLASS_NAME);
    let mut wc: WNDCLASSEXW = unsafe { zeroed() };
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.style = CS_DROPSHADOW;
    wc.lpfnWndProc = Some(wnd_proc);
    wc.hInstance = hinstance;
    wc.hCursor = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) };
    wc.hbrBackground = std::ptr::null_mut();
    wc.lpszClassName = cls.as_ptr();
    unsafe { RegisterClassExW(&wc) != 0 }
}

/// 窗口尺寸（逻辑像素 × DPI 缩放）。
pub fn window_size(dpi: u32) -> (i32, i32) {
    let d = if dpi == 0 { 96 } else { dpi } as i32;
    (W * d / 96, H * d / 96)
}

/// 显示启动动画。失败时静默返回 `None`：动画可有可无，绝不能因为它影响启动。
pub fn show(owner: HWND, hinstance: HINSTANCE) -> Option<HWND> {
    let dpi = {
        let d = unsafe { GetDpiForSystem() };
        if d == 0 {
            96
        } else {
            d
        }
    };
    let (w, h) = window_size(dpi);
    let (x, y) = crate::win::screen::center_on_primary(w, h);

    // 所有权必须交给窗口：`Box` 若留在栈上，show() 返回时就会被释放，
    // 而窗口的 GWLP_USERDATA 仍指向那块内存 —— WM_TIMER 一来就是 use-after-free。
    // 这正是"启动动画永远停在 alpha=0、窗口不消失、有时直接崩进程"的根因。
    let ptr = Box::into_raw(Box::new(Splash {
        hwnd: std::ptr::null_mut(),
        owner,
        fonts: Fonts::create(&Metrics::new(dpi)),
        metrics: Metrics::new(dpi),
        started_at: std::time::Instant::now(),
        base_x: x,
        base_y: y,
        w,
        h,
        mem: None,
    }));
    let cls = to_wide(CLASS_NAME);
    let title = to_wide(crate::APP_TITLE);
    let ex_style = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            cls.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            x,
            y + SLIDE_PX,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            ptr as *const core::ffi::c_void,
        )
    };
    if hwnd.is_null() {
        // 创建失败：把所有权收回来，别泄漏。
        drop(unsafe { Box::from_raw(ptr) });
        crate::app::diag_msg(format_args!("splash: 创建失败"));
        return None;
    }
    crate::app::diag_msg(format_args!("splash: 已显示 {:?} {w}x{h}", hwnd));
    unsafe {
        SetLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Some(hwnd)
}

/// 离屏渲染某一时刻的画面（供 `--render-splash` 验证视觉，不显示窗口）。
pub fn render_preview(at_ms: i64) -> Option<(i32, i32, Vec<u8>)> {
    let dpi = {
        let d = unsafe { GetDpiForSystem() };
        if d == 0 {
            96
        } else {
            d
        }
    };
    let (w, h) = window_size(dpi);
    let metrics = Metrics::new(dpi);
    let st = Splash {
        hwnd: std::ptr::null_mut(),
        owner: std::ptr::null_mut(),
        fonts: Fonts::create(&metrics),
        metrics,
        started_at: std::time::Instant::now()
            - std::time::Duration::from_millis(at_ms.max(0) as u64),
        base_x: 0,
        base_y: 0,
        w,
        h,
        mem: None,
    };
    let mut mem = MemDc::new(w, h)?;
    st.draw(mem.dc(), w, h);
    Some((w, h, mem.to_rgba()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_in_and_out_within_duration() {
        assert_eq!(alpha_at(0), 0);
        assert!(alpha_at(50) > 0 && alpha_at(50) < 255);
        assert!(alpha_at(160) > 200, "淡入尾段应接近不透明");
        assert_eq!(alpha_at(FADE_IN_MS + 10), 255, "保持段完全不透明");
        assert_eq!(alpha_at(FADE_OUT_AT_MS - 1), 255);
        assert!(alpha_at(FADE_OUT_AT_MS + 60) < 255);
        assert!(alpha_at(DURATION_MS - 1) < 40, "末尾几乎透明");
        assert_eq!(alpha_at(DURATION_MS), 0);
    }

    #[test]
    fn progress_sweeps_once_then_holds() {
        assert_eq!(progress_at(0), 0.0);
        assert!(progress_at(FADE_IN_MS + 100) > 0.0);
        assert_eq!(progress_at(FADE_OUT_AT_MS), 1.0);
        assert_eq!(progress_at(DURATION_MS), 1.0, "结束后不再变化");
    }

    #[test]
    fn slide_lands_exactly_at_zero() {
        assert_eq!(slide_at(0), SLIDE_PX);
        assert_eq!(slide_at(FADE_IN_MS), 0);
        assert_eq!(slide_at(DURATION_MS), 0);
        // 单调下沉：越接近结束，偏移越小
        assert!(slide_at(60) < slide_at(20));
    }

    #[test]
    fn finishes_after_duration() {
        assert!(!is_finished(DURATION_MS - 1));
        assert!(is_finished(DURATION_MS));
    }
}
