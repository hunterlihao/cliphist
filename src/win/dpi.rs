//! DPI 感知：Per-Monitor V2 + 尺寸换算。
//!
//! 不做 DPI 感知的后果是弹窗在 150% 缩放下被系统位图拉伸，字发虚、行高错位 ——
//! 对"每一条都要快速扫读"的历史列表来说这是致命的。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};

pub const BASE_DPI: u32 = 96;

/// 进程级 DPI 感知。必须在创建窗口之前调用。
///
/// 失败（例如已被清单文件固定）不致命：返回 false 让调用方继续走系统缩放。
pub fn set_process_dpi_awareness() -> bool {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 }
}

/// 窗口所在显示器的 DPI；拿不到时回落到 96。
pub fn dpi_of_window(hwnd: HWND) -> u32 {
    let d = unsafe { GetDpiForWindow(hwnd) };
    if d == 0 { BASE_DPI } else { d }
}

pub fn system_dpi() -> u32 {
    let d = unsafe { GetDpiForSystem() };
    if d == 0 { BASE_DPI } else { d }
}

/// 逻辑像素 → 物理像素（四舍五入）。
pub fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi as i64 + (BASE_DPI as i64 / 2)) / BASE_DPI as i64) as i32
}

/// 逻辑像素 → 物理像素（浮点输入，例如 1.5px 的分隔线）。
pub fn scale_f(value: f32, dpi: u32) -> i32 {
    (value * dpi as f32 / BASE_DPI as f32).round() as i32
}

/// 物理像素 → 逻辑像素（用于把鼠标位置换算回布局坐标系）。
pub fn unscale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * BASE_DPI as i64) / dpi as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_rounds_to_nearest() {
        assert_eq!(scale(100, 96), 100);
        assert_eq!(scale(100, 144), 150);
        assert_eq!(scale(1, 96), 1);
        assert_eq!(scale(0, 144), 0);
        // 小数舍入：1px @ 120dpi = 1.25 → 1
        assert_eq!(scale(1, 120), 1);
        assert_eq!(scale(5, 120), 6, "6.25 → 6");
    }

    #[test]
    fn scale_then_unscale_is_within_one_pixel() {
        for dpi in [96u32, 120, 144, 168, 192, 240] {
            for v in [1i32, 7, 40, 200, 560] {
                let round = unscale(scale(v, dpi), dpi);
                assert!((round - v).abs() <= 2, "dpi={dpi} v={v} round={round}");
            }
        }
    }

    #[test]
    fn float_scaling() {
        assert_eq!(scale_f(1.5, 96), 2);
        assert_eq!(scale_f(1.5, 192), 3);
    }
}
