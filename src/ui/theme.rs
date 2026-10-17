//! 主题令牌与度量化 —— 方向：「索引页」。
//!
//! ## 为什么是这个方向
//!
//! 剪贴板本质上是一叠纸片：你抄过什么、什么时候抄的。所以这个界面按**索引页**来排，
//! 而不是按「控制面板」来排：安静的外边距、一种强排版语气、时间像书页边缘的页码那样
//! 竖着对齐，而不是把每行塞满图标和彩色徽标。
//!
//! ## 刻意承担的那一个风险
//!
//! **大多数行没有任何图标。** 文本行左边是一段空着的边距；只有图片行放缩略图、
//! 文件行放一个小文件夹轮廓。理由：一行一个灰色"文本/文件/图片"方块是纯粹的噪声 ——
//! 十行里八个一模一样，眼睛要先过滤掉它们才能读内容。真正决定"这是不是我要那段话"的
//! 是预览文字，不是类型标签。代价是每行牺牲一点横向空间，换来一眼能扫的版面。
//!
//! 与之配套的第二个决定：时间（"3 分钟前"）走等宽、右对齐，形成一条干净的竖线，
//! 于是整列看起来像一本书的边缘注记，而不是一堆参差的句子。
//!
//! 别处的克制：反相选中（墨底白字）是唯一张扬的笔触，其余一律发丝线与灰阶。
//! 没有彩色强调色、没有圆角卡片、没有阴影（浮层除外）、没有进度条式的装饰。
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
    /// 主文字 / 结构线里最深的一级
    pub const INK: COLORREF = c(11, 11, 12);
    /// 次级文字（来源进程、元信息）
    pub const INK2: COLORREF = c(85, 85, 92);
    /// 三级文字（时间、提示、空态说明）
    pub const INK3: COLORREF = c(142, 142, 150);
    /// 发丝分隔线
    pub const RULE: COLORREF = c(230, 230, 234);
    /// 交互对象描边（输入框下划线、文件夹轮廓）
    pub const RULE_STRONG: COLORREF = c(201, 201, 208);
    /// 悬停底：比纸面深 2%，轻到几乎看不见，只用来确认"鼠标在这行"
    pub const HOVER: COLORREF = c(250, 250, 251);
    /// 选中行左边缘的墨条（3px），像编辑器里的当前行标记。
    ///
    /// 选中**不改底色、也不反相**：整行仍然是纸白，焦点只靠这一道结构性的墨条表达。
    /// 黑块在长列表里太重，会把两行文字压成一片；浅灰底又等于"另一种底色"。
    pub const SEL_EDGE: COLORREF = c(11, 11, 12);
    /// 警示（"未记录"、热键冲突、清空确认）
    pub const WARN: COLORREF = c(176, 62, 44);
}

pub mod font {
    /// 界面 / 预览。中文界面在 Windows 上基本只有雅黑能看，故首选雅黑。
    pub const UI: &str = "Microsoft YaHei UI";
    pub const UI_FALLBACK: &str = "Segoe UI";
    /// 数据字体：时间、计数、尺寸、路径 —— 一律等宽，才能竖着对齐。
    pub const MONO: &str = "Consolas";
}

/// 弹窗与设置窗口的全部尺寸。
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub dpi: u32,
    /// 弹窗宽度
    pub width: i32,
    /// 弹窗最大高度（含搜索行与页脚）
    pub max_height: i32,
    pub min_height: i32,
    /// 主窗口顶部标题栏高度（弹窗为 0：弹窗没有标题栏）
    pub title_h: i32,
    /// 顶部搜索行高度（含下划线）
    pub header_h: i32,
    pub row_h: i32,
    /// 缩略图 / 文件夹标记的方框边长
    pub mark: i32,
    pub footer_h: i32,
    pub pad: i32,
    pub gap: i32,
    pub scrollbar_w: i32,
    pub fs_preview: i32,
    pub fs_meta: i32,
    pub fs_label: i32,
    pub fs_mono: i32,
}

impl Metrics {
    pub fn new(dpi: u32) -> Self {
        Self {
            dpi,
            width: s(460, dpi),
            max_height: s(600, dpi),
            min_height: s(160, dpi),
            title_h: 0,
            header_h: s(42, dpi),
            row_h: s(56, dpi),
            mark: s(34, dpi),
            footer_h: s(28, dpi),
            pad: s(16, dpi),
            gap: s(12, dpi),
            scrollbar_w: s(3, dpi),
            fs_preview: s(15, dpi),
            fs_meta: s(11, dpi),
            fs_label: s(11, dpi),
            fs_mono: s(11, dpi),
        }
    }

    /// 主窗口（居中大窗）：同一套排版，宽度更大、高度固定，顶部多一条标题栏。
    ///
    /// 不复用弹窗的自适应高度：主窗口是个"可以待着"的窗口，
    /// 高度随着记录条数跳动会显得很廉价。
    pub fn main_window(dpi: u32) -> Self {
        let mut m = Self::new(dpi);
        m.width = s(760, dpi);
        m.max_height = s(620, dpi);
        m.min_height = s(620, dpi);
        m.title_h = s(38, dpi);
        m
    }

    /// 列表区顶部 = 标题栏 + 搜索行。
    pub fn list_top(&self) -> i32 {
        self.title_h + self.header_h
    }

    /// 文字区左边界：外边距 + 标记方框 + 间隙。文本行虽然不画标记，
    /// 但也从同一条线开始 —— 空出来的那一段就是"边距"，而不是省的空白。
    pub fn text_left(&self) -> i32 {
        self.pad + self.mark + self.gap
    }

    /// 文字区右边界。
    pub fn text_right(&self) -> i32 {
        self.width - self.pad
    }

    /// 列表可视区高度（扣掉标题栏、搜索行与页脚）。
    pub fn list_height(&self) -> i32 {
        (self.max_height - self.list_top() - self.footer_h).max(self.row_h)
    }

    /// 一屏能放几行（至少 1）。
    pub fn visible_rows(&self) -> i32 {
        (self.list_height() / self.row_h).max(1)
    }

    /// 按条目数自适应窗口高度：1 条记录时不摆一大片空白。
    pub fn height_for(&self, rows: usize) -> i32 {
        let want = self.list_top() + self.footer_h + (rows as i32).max(1) * self.row_h;
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
    }

    #[test]
    fn selection_edge_is_ink_and_keeps_paper_background() {
        // 选中态：底色仍是纸白（只有悬停有极浅的底），焦点由左边缘墨条表达。
        assert_eq!(color::SEL_EDGE, color::INK);
        let lum = |c: u32| (c & 0xFF) + ((c >> 8) & 0xFF) + ((c >> 16) & 0xFF);
        assert!(lum(color::PAPER) > lum(color::HOVER), "悬停必须比纸面略深");
        assert_eq!(lum(color::PAPER), 765, "纸面必须是纯白");
    }

    #[test]
    fn metrics_scale_with_dpi() {
        let m = Metrics::new(96);
        assert_eq!(m.width, 460);
        assert_eq!(m.row_h, 56);
        assert_eq!(m.header_h, 42);
        let m2 = Metrics::new(144);
        assert_eq!(m2.width, 690);
        assert_eq!(m2.row_h, 84);
    }

    #[test]
    fn metrics_are_always_positive_and_consistent() {
        for dpi in [96u32, 120, 144, 192, 240, 288] {
            let m = Metrics::new(dpi);
            assert!(m.width > 0 && m.row_h > 0 && m.mark > 0 && m.pad > 0);
            assert!(m.mark < m.row_h, "标记方框必须比行高矮，否则会顶出边界");
            assert!(m.header_h > 0 && m.footer_h > 0);
            // 字号层级：预览 > 元信息 >= 等宽 >= 标签
            assert!(m.fs_preview > m.fs_meta);
            assert!(m.fs_meta >= m.fs_mono);
            assert!(m.fs_mono >= m.fs_label);
            // 文字区必须真的有宽度，且不越过右边界
            assert!(m.text_left() < m.text_right() - 100, "dpi={dpi} 文字区太窄");
            assert!(m.text_left() > m.pad, "文字区必须让出左侧边距");
        }
    }

    #[test]
    fn visible_rows_and_height_clamping() {
        let m = Metrics::new(96);
        assert!(m.visible_rows() >= 8);
        assert_eq!(m.height_for(0), m.min_height);
        assert_eq!(m.height_for(1_000_000), m.max_height);
        // 3 行时高度 = 42 + 28 + 3*56 = 238，落在 [min, max] 内
        assert_eq!(m.height_for(3), 238);
    }
}
