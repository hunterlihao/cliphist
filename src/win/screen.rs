//! 屏幕与显示器的小工具：主显示器工作区、居中定位。

use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};

/// 主显示器的**可用工作区**（不含任务栏）。
///
/// 用工作区而不是整块屏幕：否则居中后窗口会压在任务栏下面。
pub fn primary_work_area() -> RECT {
    let pt = POINT { x: 0, y: 0 };
    unsafe {
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi) != 0 {
            return mi.rcWork;
        }
    }
    // 拿不到就退一个保守的默认值，保证窗口至少放得下。
    RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    }
}

/// 在屏幕上**绝对居中**的左上角坐标（主显示器工作区）。
pub fn center_on_primary(width: i32, height: i32) -> (i32, i32) {
    let wa = primary_work_area();
    let x = wa.left + ((wa.right - wa.left) - width).max(0) / 2;
    let y = wa.top + ((wa.bottom - wa.top) - height).max(0) / 2;
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_area_is_sane() {
        let wa = primary_work_area();
        assert!(wa.right > wa.left && wa.bottom > wa.top);
        // 任何 1080p 以上的机器都该满足：工作区高度至少 600 逻辑像素
        assert!(wa.bottom - wa.top >= 600, "工作区太小");
    }

    #[test]
    fn centering_keeps_window_inside_work_area() {
        let wa = primary_work_area();
        let (w, h) = (760, 640);
        let (x, y) = center_on_primary(w, h);
        assert!(x >= wa.left && x + w <= wa.right, "x={x} 越界");
        assert!(y >= wa.top && y + h <= wa.bottom, "y={y} 越界");
        // 真实居中：左右、上下余量相差不超过 1px（奇数尺寸的取整误差）
        let left_gap = x - wa.left;
        let right_gap = wa.right - (x + w);
        assert!(
            (left_gap - right_gap).abs() <= 1,
            "{left_gap} vs {right_gap}"
        );
        let top_gap = y - wa.top;
        let bottom_gap = wa.bottom - (y + h);
        assert!(
            (top_gap - bottom_gap).abs() <= 1,
            "{top_gap} vs {bottom_gap}"
        );
    }

    #[test]
    fn oversized_window_is_clamped_to_top_left() {
        let wa = primary_work_area();
        let (x, y) = center_on_primary(100_000, 100_000);
        assert_eq!((x, y), (wa.left, wa.top));
    }
}
