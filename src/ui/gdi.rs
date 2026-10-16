//! GDI 绘制原语：字体缓存、双缓冲、文字/形状绘制、缩略图缓存。
//!
//! 这里刻意**不引入 comctl32**：没有 ListView、没有 ImageList，列表自己画。
//! 代价是代码量，收益是内存（少加载一个 DLL、少一层控件对象）和完全可控的视觉，
//! 并且不需要为 comctl32 v6 清单文件操心。

use windows_sys::Win32::Foundation::{COLORREF, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    AlphaBlend, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    CreatePen, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, FillRect, GetDC,
    GetStockObject, GetTextExtentPoint32W, GetTextMetricsW, ReleaseDC, RoundRect, SelectObject,
    SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_GUI_FONT, DIB_RGB_COLORS, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_BOLD,
    FW_NORMAL, HBITMAP, HBRUSH, HDC, HFONT, HGDIOBJ, HPEN, OUT_DEFAULT_PRECIS, PS_SOLID, SRCCOPY,
    TEXTMETRICW, TRANSPARENT, VARIABLE_PITCH,
};

use crate::image::thumb::decode_png_rgba;
use crate::ui::theme::{font, Metrics};
use crate::win::strings::to_wide;

const BI_RGB: u32 = 0;

/// 字体集合：每个 DPI 创建一次，随窗口一起释放。
pub struct Fonts {
    pub ui: HFONT,
    pub ui_bold: HFONT,
    pub meta: HFONT,
    pub badge: HFONT,
    pub mono: HFONT,
    /// 记录哪些是自建字体（需要 DeleteObject），哪些是系统库存对象（不能删）。
    owned: Vec<HFONT>,
}

impl Fonts {
    pub fn create(m: &Metrics) -> Fonts {
        let mut owned = Vec::new();
        let mut make = |px: i32, weight: u32, face: &str| -> HFONT {
            let f = create_font(face, px, weight, false);
            if !f.is_null() {
                owned.push(f);
                f
            } else {
                unsafe { GetStockObject(DEFAULT_GUI_FONT) as HFONT }
            }
        };
        let ui = make(m.fs_preview, FW_NORMAL, font::UI);
        let ui_bold = make(m.fs_preview, FW_BOLD, font::UI);
        let meta = make(m.fs_meta, FW_NORMAL, font::UI);
        let badge = make(m.fs_badge, FW_NORMAL, font::UI);
        let mono = make(m.fs_mono, FW_NORMAL, font::MONO);
        Fonts {
            ui,
            ui_bold,
            meta,
            badge,
            mono,
            owned,
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in self.owned.drain(..) {
            unsafe { DeleteObject(f as HGDIOBJ) };
        }
    }
}

/// 创建字体。失败返回空指针，由调用方回落到系统字体。
pub fn create_font(face: &str, px: i32, weight: u32, italic: bool) -> HFONT {
    let wide = to_wide(face);
    unsafe {
        CreateFontW(
            -px, // 负值 = 字符高度（正值是"单元格高度"，中文会明显偏高）
            0,
            0,
            0,
            weight as i32,
            italic as u32,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            (VARIABLE_PITCH | FF_DONTCARE) as u32,
            wide.as_ptr(),
        )
    }
}

/// 离屏位图 + 内存 DC：整个窗口一次画完再 `BitBlt` 上屏，消除闪烁。
pub struct MemDc {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    w: i32,
    h: i32,
}

impl MemDc {
    pub fn new(w: i32, h: i32) -> Option<MemDc> {
        if w <= 0 || h <= 0 {
            return None;
        }
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return None;
            }
            let dc = CreateCompatibleDC(screen);
            let bmp = if dc.is_null() {
                std::ptr::null_mut()
            } else {
                CreateCompatibleBitmap(screen, w, h)
            };
            ReleaseDC(std::ptr::null_mut(), screen);
            if dc.is_null() || bmp.is_null() {
                if !dc.is_null() {
                    DeleteDC(dc);
                }
                if !bmp.is_null() {
                    DeleteObject(bmp as HGDIOBJ);
                }
                return None;
            }
            let old = SelectObject(dc, bmp as HGDIOBJ);
            Some(MemDc { dc, bmp, old, w, h })
        }
    }

    pub fn dc(&self) -> HDC {
        self.dc
    }

    pub fn size(&self) -> (i32, i32) {
        (self.w, self.h)
    }

    /// 尺寸变了就重建位图。返回是否可用。
    pub fn resize(&mut self, w: i32, h: i32) -> bool {
        if w == self.w && h == self.h {
            return true;
        }
        if w <= 0 || h <= 0 {
            return false;
        }
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return false;
            }
            let bmp = CreateCompatibleBitmap(screen, w, h);
            ReleaseDC(std::ptr::null_mut(), screen);
            if bmp.is_null() {
                return false;
            }
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp as HGDIOBJ);
            self.old = SelectObject(self.dc, bmp as HGDIOBJ);
            self.bmp = bmp;
            self.w = w;
            self.h = h;
        }
        true
    }

    pub fn blit_to(&self, dst: HDC) {
        unsafe {
            BitBlt(dst, 0, 0, self.w, self.h, self.dc, 0, 0, SRCCOPY);
        }
    }
}

impl Drop for MemDc {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp as HGDIOBJ);
            DeleteDC(self.dc);
        }
    }
}

// ------------------------------------------------------------ 绘制原语

pub fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

pub fn fill(dc: HDC, rc: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, rc, brush);
        DeleteObject(brush as HGDIOBJ);
    }
}

/// 1px 矩形描边（画在 `rc` 内侧）。
pub fn frame(dc: HDC, rc: &RECT, color: COLORREF) {
    fill(dc, &rect(rc.left, rc.top, rc.right, rc.top + 1), color);
    fill(
        dc,
        &rect(rc.left, rc.bottom - 1, rc.right, rc.bottom),
        color,
    );
    fill(dc, &rect(rc.left, rc.top, rc.left + 1, rc.bottom), color);
    fill(dc, &rect(rc.right - 1, rc.top, rc.right, rc.bottom), color);
}

/// 发丝横线。
pub fn hline(dc: HDC, x0: i32, x1: i32, y: i32, color: COLORREF) {
    fill(dc, &rect(x0, y, x1, y + 1), color);
}

pub fn vline(dc: HDC, x: i32, y0: i32, y1: i32, color: COLORREF) {
    fill(dc, &rect(x, y0, x + 1, y1), color);
}

pub fn round_fill(dc: HDC, rc: &RECT, color: COLORREF, radius: i32) {
    unsafe {
        let brush: HBRUSH = CreateSolidBrush(color);
        let pen: HPEN = CreatePen(PS_SOLID, 1, color);
        let ob = SelectObject(dc, brush as HGDIOBJ);
        let op = SelectObject(dc, pen as HGDIOBJ);
        RoundRect(
            dc,
            rc.left,
            rc.top,
            rc.right,
            rc.bottom,
            radius * 2,
            radius * 2,
        );
        SelectObject(dc, ob);
        SelectObject(dc, op);
        DeleteObject(brush as HGDIOBJ);
        DeleteObject(pen as HGDIOBJ);
    }
}

/// 实心圆点（置顶标记用）。
pub fn dot(dc: HDC, cx: i32, cy: i32, r: i32, color: COLORREF) {
    let rc = rect(cx - r, cy - r, cx + r, cy + r);
    round_fill(dc, &rc, color, r);
}

/// 单行文字（自动省略号截断）。
pub fn text(dc: HDC, f: HFONT, color: COLORREF, rc: &RECT, s: &str, center: bool) -> i32 {
    if s.is_empty() {
        return 0;
    }
    let wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let old = SelectObject(dc, f as HGDIOBJ);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, color);
        let mut r = *rc;
        let mut flags = DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX;
        if center {
            flags |= DT_CENTER;
        } else {
            flags |= DT_LEFT;
        }
        let h = DrawTextW(dc, wide.as_ptr(), wide.len() as i32, &mut r, flags);
        SelectObject(dc, old);
        h
    }
}

/// 文本像素宽度。
pub fn text_width(dc: HDC, f: HFONT, s: &str) -> i32 {
    let wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let old = SelectObject(dc, f as HGDIOBJ);
        let mut size = windows_sys::Win32::Foundation::SIZE { cx: 0, cy: 0 };
        GetTextExtentPoint32W(dc, wide.as_ptr(), wide.len() as i32, &mut size);
        SelectObject(dc, old);
        size.cx
    }
}

/// 行高（用于垂直居中）。
pub fn text_height(dc: HDC, f: HFONT) -> i32 {
    unsafe {
        let old = SelectObject(dc, f as HGDIOBJ);
        let mut tm: TEXTMETRICW = std::mem::zeroed();
        GetTextMetricsW(dc, &mut tm);
        SelectObject(dc, old);
        tm.tmHeight
    }
}

// ------------------------------------------------------------ 缩略图

/// 已解码的缩略图位图（32bpp 预乘 alpha，供 `AlphaBlend` 使用）。
struct Thumb {
    id: i64,
    hdc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    w: i32,
    h: i32,
    last_used: u64,
}

impl Drop for Thumb {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.hdc, self.old);
            DeleteObject(self.bmp as HGDIOBJ);
            DeleteDC(self.hdc);
        }
    }
}

/// 缩略图 LRU 缓存。
///
/// 上限刻意压得很小（默认 24 张）：一屏最多也就十来行，缓存再多只是白占内存。
/// 这是"内存与历史条数解耦"的另一半 —— 1000 条历史也只有个位数的缩略图常驻。
pub struct ThumbCache {
    items: Vec<Thumb>,
    max: usize,
    tick: u64,
    hits: u64,
    misses: u64,
}

impl ThumbCache {
    pub fn new(max: usize) -> Self {
        Self {
            items: Vec::new(),
            max: max.max(4),
            tick: 0,
            hits: 0,
            misses: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn contains(&self, id: i64) -> bool {
        self.items.iter().any(|t| t.id == id)
    }

    /// 取缩略图；未缓存则用 PNG 解码后插入。
    pub fn get_or_insert(&mut self, id: i64, png: &[u8]) -> Option<()> {
        self.tick += 1;
        let tick = self.tick;
        if let Some(t) = self.items.iter_mut().find(|t| t.id == id) {
            t.last_used = tick;
            self.hits += 1;
            return Some(());
        }
        self.misses += 1;
        let img = decode_png_rgba(png).ok()?;
        let t = create_thumb(id, &img)?;
        if self.items.len() >= self.max {
            // 淘汰最久未用的那张
            if let Some((idx, _)) = self
                .items
                .iter()
                .enumerate()
                .min_by_key(|(_, t)| t.last_used)
            {
                self.items.swap_remove(idx);
            }
        }
        self.items.push(t);
        Some(())
    }

    /// 把缓存的缩略图按"适配 box"的方式画到目标 DC。
    pub fn draw(&self, dc: HDC, id: i64, box_rect: &RECT) -> bool {
        let Some(t) = self.items.iter().find(|t| t.id == id) else {
            return false;
        };
        let bw = box_rect.right - box_rect.left;
        let bh = box_rect.bottom - box_rect.top;
        if bw <= 0 || bh <= 0 || t.w <= 0 || t.h <= 0 {
            return false;
        }
        // 等比缩放并居中
        let scale = (bw as f32 / t.w as f32).min(bh as f32 / t.h as f32);
        let dw = ((t.w as f32 * scale).round() as i32).clamp(1, bw);
        let dh = ((t.h as f32 * scale).round() as i32).clamp(1, bh);
        let dx = box_rect.left + (bw - dw) / 2;
        let dy = box_rect.top + (bh - dh) / 2;

        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe { AlphaBlend(dc, dx, dy, dw, dh, t.hdc, 0, 0, t.w, t.h, blend) != 0 }
    }
}

/// 由 RGBA 创建缩略图位图。GDI 的 `AlphaBlend` 要求**预乘 alpha**。
fn create_thumb(id: i64, img: &crate::core::model::ImageData) -> Option<Thumb> {
    let w = img.width as i32;
    let h = img.height as i32;
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return None;
        }
        let hdc = CreateCompatibleDC(screen);
        ReleaseDC(std::ptr::null_mut(), screen);
        if hdc.is_null() {
            return None;
        }

        let bmi = windows_sys::Win32::Graphics::Gdi::BITMAPINFO {
            bmiHeader: windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER {
                biSize: std::mem::size_of::<windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER>()
                    as u32,
                biWidth: w,
                biHeight: -h, // 自上而下
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [windows_sys::Win32::Graphics::Gdi::RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(
            hdc,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if bmp.is_null() || bits.is_null() {
            DeleteDC(hdc);
            return None;
        }
        let dst = bits as *mut u8;
        for i in 0..(w * h) as usize {
            let s = i * 4;
            let a = img.rgba[s + 3] as u32;
            // 预乘：c' = c * a / 255
            let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            *dst.add(s) = pm(img.rgba[s + 2]); // B
            *dst.add(s + 1) = pm(img.rgba[s + 1]); // G
            *dst.add(s + 2) = pm(img.rgba[s]); // R
            *dst.add(s + 3) = a as u8;
        }

        let old = SelectObject(hdc, bmp as HGDIOBJ);
        Some(Thumb {
            id,
            hdc,
            bmp,
            old,
            w,
            h,
            last_used: 0,
        })
    }
}

/// 供其他模块使用的类型别名（避免到处写 `windows_sys::...`）。
pub type DrawDc = HDC;
pub type Brush = HBRUSH;
pub type Pen = HPEN;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::thumb::encode_png_rgba;
    use crate::ui::theme::Metrics;

    #[test]
    fn memo_dc_lifecycle() {
        let mut m = MemDc::new(64, 32).expect("创建内存 DC 失败");
        assert_eq!(m.size(), (64, 32));
        assert!(m.resize(64, 32), "同尺寸不应重建失败");
        assert!(m.resize(100, 50));
        assert_eq!(m.size(), (100, 50));
        assert!(!m.resize(0, 10), "非法尺寸必须拒绝而不是崩溃");
        assert_eq!(m.size(), (100, 50));
    }

    #[test]
    fn font_creation_returns_handle_or_null_but_never_panics() {
        let m = Metrics::new(96);
        let fonts = Fonts::create(&m);
        assert!(!fonts.ui.is_null());
        assert!(!fonts.mono.is_null());
    }

    #[test]
    fn thumb_cache_evicts_and_respects_max() {
        let img = crate::core::model::ImageData {
            width: 8,
            height: 8,
            rgba: vec![255u8; 8 * 8 * 4],
        };
        let png = encode_png_rgba(img.width, img.height, &img.rgba).unwrap();
        let mut cache = ThumbCache::new(4);
        for id in 0..10 {
            assert!(cache.get_or_insert(id, &png).is_some());
        }
        assert!(cache.len() <= 4, "LRU 上限失效：{}", cache.len());
        // 命中一次已缓存的
        let last = 9;
        assert!(cache.contains(last));
        let before = cache.stats();
        cache.get_or_insert(last, &png);
        assert_eq!(cache.stats().0, before.0 + 1, "应记一次命中");
    }

    #[test]
    fn thumb_cache_tolerates_bad_png() {
        let mut cache = ThumbCache::new(4);
        assert!(cache.get_or_insert(1, b"not a png").is_none());
        assert!(cache.is_empty());
    }

    #[test]
    fn thumb_draw_without_entry_is_false() {
        let m = MemDc::new(100, 100).unwrap();
        let cache = ThumbCache::new(4);
        assert!(!cache.draw(m.dc(), 42, &rect(0, 0, 40, 40)));
    }
}
