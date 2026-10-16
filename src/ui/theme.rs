//! 主题令牌与度量化。
//!
//! 设计取向（与同机 `topmost-pin` 的设计语言一致）：**近单色**。
//! 列表里只有纸（白）与墨（近黑）两级，选中态用"反相"表达 —— 这是单色下最清楚的
//! 表达方式：不需要彩色，也不会因为强调色而让"文本/文件/图片"三类混成一片脏色。
//!
//! 所有尺寸都是**逻辑像素**，由 [`Metrics::new`] 按 DPI 换算成物理像素。

use windows_sys::Win32::Foundation::COLORREF;

/// 逻辑像素 + DPI → 物理像素（四舍五入）。
fn s(v: i32, dpi: u32) -> i32 {
    ((v as i64 * dpi as i64 + 48) / 96) as i32
}

/// `COLORREF`：0x00BBGGRR（注意不是 RGB）。
pub const fn c(r: u8, g: u8, b: u8) -> COLORREF {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

pub mod color {
    use super::c;
    use windows_sys::Win32::Foundation::COLORREF;

    /// 纸面
    pub const PAPER: COLORREF = c(255, 255, 255);
    /// 凹陷面：搜索框、键帽、页脚
    pub const WELL: COLORREF = c(244, 244, 244);
    /// 主文字
    pub const INK: COLORREF = c(10, 10, 10);
    /// 次级文字（来源进程）
    pub const INK2: COLORREF = c(74, 74, 74);
    /// 三级文字（时间、提示）
    pub const INK3: COLORREF = c(150, 150, 150);
    /// 发丝分隔线
    pub const RULE: COLORREF = c(232, 232, 232);
    /// 交互对象描边
    pub const RULE_STRONG: COLORREF = c(198, 198, 198);
    /// 选中行：反相底
    pub const SEL_BG: COLORREF = c(10, 10, 10);
    pub const SEL_FG: COLORREF = c(255, 255, 255);
    pub const SEL_FG2: COLORREF = c(168, 168, 168);
    /// 悬停（未选中）
    pub const HOVER: COLORREF = c(246, 246, 246);
    /// 类型徽标底
    pub const BADGE_BG: COLORREF = c(234, 234, 234);
    pub const BADGE_FG: COLORREF = c(96, 96, 96);
    /// 置顶标记
    pub const PIN: COLORREF = c(10, 10, 10);
    /// 警示（"未记录""热键冲突"）
    pub const WARN: COLORREF = c(176, 62, 44);
}

pub mod font {
    /// 界面/预览字体。中文界面在 Windows 上基本只有雅黑能看，故首选雅黑。
    pub const UI: &str = "Microsoft YaHei UI";
    pub const UI_FALLBACK: &str = "Segoe UI";
    /// 数据字体：一律走等宽，让时间、尺寸、路径能竖着对齐扫读。
    pub const MONO: &str = "Consolas";
}

/// 弹窗与设置窗口的全部尺寸。
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub dpi: u32,
    /// 弹窗宽度
    pub width: i32,
    /// 弹窗最大高度（含搜索框与页脚）
    pub max_height: i32,
    pub min_height: i32,
    pub search_h: i32,
    pub row_h: i32,
    /// 缩略图/徽标方块边长
    pub thumb: i32,
    pub footer_h: i32,
    pub pad: i32,
    pub gap: i32,
    pub radius: i32,
    pub scrollbar_w: i32,
    pub fs_preview: i32,
    pub fs_meta: i32,
    pub fs_badge: i32,
    pub fs_mono: i32,
}

impl Metrics {
    pub fn new(dpi: u32) -> Self {
        Self {
            dpi,
            width: s(420, dpi),
            max_height: s(560, dpi),
            min_height: s(140, dpi),
            search_h: s(40, dpi),
            row_h: s(52, dpi),
            thumb: s(40, dpi),
            footer_h: s(26, dpi),
            pad: s(10, dpi),
            gap: s(10, dpi),
            radius: s(4, dpi),
            scrollbar_w: s(3, dpi),
            fs_preview: s(14, dpi),
            fs_meta: s(12, dpi),
            fs_badge: s(11, dpi),
            fs_mono: s(12, dpi),
        }
    }

    /// 列表可视区高度（扣掉搜索框与页脚）。
    pub fn list_height(&self) -> i32 {
        (self.max_height - self.search_h - self.footer_h).max(self.row_h)
    }

    /// 一屏能放几行（至少 1）。
    pub fn visible_rows(&self) -> i32 {
        (self.list_height() / self.row_h).max(1)
    }

    /// 按条目数自适应窗口高度：条目少时不留一大片空白。
    pub fn height_for(&self, rows: usize) -> i32 {
        let want = self.search_h + self.footer_h + (rows as i32).max(1) * self.row_h;
        want.clamp(self.min_height, self.max_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorref_is_bgr() {
        assert_eq!(color::PAPER, 0x00FF_FFFF);
        assert_eq!(c(255, 0, 0), 0x0000_00FF);
        assert_eq!(color::SEL_BG, color::INK);
    }

    #[test]
    fn metrics_scale_with_dpi() {
        let m = Metrics::new(96);
        assert_eq!(m.width, 420);
        assert_eq!(m.row_h, 52);
        let m2 = Metrics::new(144);
        assert_eq!(m2.width, 630);
        assert_eq!(m2.row_h, 78);
    }

    #[test]
    fn metrics_are_always_positive() {
        for dpi in [96u32, 120, 144, 192, 240, 288] {
            let m = Metrics::new(dpi);
            assert!(m.width > 0 && m.row_h > 0 && m.thumb > 0 && m.pad > 0);
            assert!(m.thumb < m.row_h, "缩略图必须比行高矮，否则会顶出边界");
            assert!(m.search_h > 0 && m.footer_h > 0);
            assert!(m.fs_preview > m.fs_meta && m.fs_meta >= m.fs_badge);
        }
    }

    #[test]
    fn visible_rows_and_height_clamping() {
        let m = Metrics::new(96);
        assert!(m.visible_rows() >= 1);
        assert_eq!(m.height_for(0), m.min_height);
        assert_eq!(m.height_for(1_000_000), m.max_height);
        // 3 行时高度 = 40 + 26 + 3*52 = 222，落在 [min, max] 内
        assert_eq!(m.height_for(3), 222);
    }
}
