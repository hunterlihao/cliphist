//! 窗口外观：标题栏按钮的命中（`title_hit`）与 DWM 圆角。
//!
//! 绘制和命中**共用同一个 `title_hit`** —— 之前两处各写一套，才出现“按钮画得出来却点不到”。

use super::*;

/// 窗口的两种身份。
///
/// 两者**共用同一个窗口类与同一份绘制代码**：主窗口只是多一条标题栏、宽度更大、
/// 高度固定，并且打开时在屏幕上绝对居中 —— 而不是跟着鼠标跑。
/// 这样列表的排版、命中测试、滚动、缩略图逻辑一处修改两边同时生效。
/// 主窗口标题栏上点到了哪一块。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TitleHit {
    /// 空白处：整条当系统标题栏，交给它去拖动
    Drag,
    Settings,
    Close,
    /// 不在标题栏里（或这个窗口没有标题栏）
    None,
}

/// 标题栏命中测试（纯函数，便于用单测钉住边界）。
///
/// 这里踩过一个坑：一开始整条标题栏都返回 `HTCAPTION`，结果**按钮永远点不到** ——
/// 点击被当成"拖动标题栏"由系统吃掉了，根本走不到 `WM_LBUTTONDOWN`。
/// 所以拖动区必须把右侧两个按钮挖掉。
pub fn title_hit(m: &Metrics, x: i32, y: i32) -> TitleHit {
    if m.title_h <= 0 || y < 0 || y >= m.title_h || x < 0 {
        return TitleHit::None;
    }
    if x >= m.width - m.title_h {
        return TitleHit::Close;
    }
    if x >= m.width - 2 * m.title_h {
        return TitleHit::Settings;
    }
    TitleHit::Drag
}

/// Win11 起 DWM 支持窗口圆角；失败（Win10）就返回 false，退回直角边框。
pub(super) fn apply_rounded_corners(hwnd: HWND) -> bool {
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE,
    };
    const DWMWCP_ROUND: i32 = 2;
    let pref: i32 = DWMWCP_ROUND;
    let hr = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &pref as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        )
    };
    hr == 0
}
