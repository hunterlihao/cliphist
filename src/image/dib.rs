//! `CF_DIB` / `CF_DIBV5` 与 RGBA 之间的纯字节转换。
//!
//! 这里不碰剪贴板、不碰 GDI，只处理内存布局，因此"采集到的截图"和"写回去的截图"
//! 都能用往返测试锁住（见文件末尾）。

/// 解析出的 DIB 头部信息。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DibInfo {
    pub width: i32,
    /// 正数：自下而上存储；负数：自上而下。（这是 DIB 的约定，容易搞反。）
    pub height: i32,
    pub bit_count: u16,
    pub compression: u32,
    /// 像素数据的起始偏移（跳过头部、掩码、调色板）。
    pub pixel_offset: usize,
    pub alpha_mask: u32,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
}

pub const BI_RGB: u32 = 0;
pub const BI_BITFIELDS: u32 = 3;
pub const BI_JPEG: u32 = 4;
pub const BI_PNG: u32 = 5;

/// `BITMAPV5HEADER` 大小。
pub const BITMAPV5HEADER_SIZE: usize = 124;
const BITMAPINFOHEADER_SIZE: usize = 40;

fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)?.try_into().ok().map(u32::from_le_bytes)
}

fn rd_i32(b: &[u8], off: usize) -> Option<i32> {
    b.get(off..off + 4)?.try_into().ok().map(i32::from_le_bytes)
}

fn rd_u16(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2)?.try_into().ok().map(u16::from_le_bytes)
}

/// 解析 DIB 头部。只接受本程序会处理的形态：24/32 位、未压缩或位域压缩。
pub fn parse_dib(bytes: &[u8]) -> Option<DibInfo> {
    let header_size = rd_u32(bytes, 0)? as usize;
    if header_size < BITMAPINFOHEADER_SIZE || header_size > bytes.len() {
        return None;
    }
    let width = rd_i32(bytes, 4)?;
    let height = rd_i32(bytes, 8)?;
    let bit_count = rd_u16(bytes, 14)?;
    let compression = rd_u32(bytes, 16)?;

    if compression == BI_JPEG || compression == BI_PNG {
        // 剪贴板里嵌 JPEG/PNG 流：不是裸 DIB，交回上层按注册格式处理。
        return None;
    }
    if bit_count != 24 && bit_count != 32 {
        return None;
    }
    if width <= 0 || height == 0 {
        return None;
    }

    // 掩码位置取决于头部大小：
    // - biSize == 40 且 BI_BITFIELDS：3 个 DWORD 紧跟头部（老约定，无 alpha）。
    // - biSize >= 108（V4/V5）：掩码在头部内部的 40..56 偏移处。
    let (red, green, blue, alpha, mask_bytes) = if header_size >= 108 {
        (
            rd_u32(bytes, 40)?,
            rd_u32(bytes, 44)?,
            rd_u32(bytes, 48)?,
            rd_u32(bytes, 52)?,
            0usize,
        )
    } else if compression == BI_BITFIELDS {
        (
            rd_u32(bytes, 40)?,
            rd_u32(bytes, 44)?,
            rd_u32(bytes, 48)?,
            0,
            12usize,
        )
    } else {
        // 32 位 BI_RGB：第 4 字节在实践中既可能是保留位也可能是真 alpha，
        // 先按 BGRA 取出来，再在 dib_to_rgba 里用"全 0 即不透明"规则兜底。
        (0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000, 0)
    };

    // 有些截图工具写的是 V4/V5 头，却把 RGB 掩码字段全部留成 0（语义上等于"默认 BGRA"）。
    // 若照掩码取值，`extract(v, 0)` 恒为 0 —— 整张图变成**纯黑**。
    // 这就是"截图复制进来变成黑图"的根因，必须回落到默认 BGRA。
    let (red, green, blue, alpha) = if red == 0 && green == 0 && blue == 0 {
        (
            0x00FF_0000,
            0x0000_FF00,
            0x0000_00FF,
            if bit_count == 32 { 0xFF00_0000 } else { 0 },
        )
    } else {
        (red, green, blue, alpha)
    };

    let pixel_offset = header_size + mask_bytes;
    if pixel_offset > bytes.len() {
        return None;
    }

    Some(DibInfo {
        width,
        height,
        bit_count,
        compression,
        pixel_offset,
        alpha_mask: alpha,
        red_mask: red,
        green_mask: green,
        blue_mask: blue,
    })
}

/// 按掩码取分量并线性拉伸到 0..255。
#[inline]
fn extract(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = mask >> shift;
    let v = (pixel & mask) >> shift;
    if max == 0 {
        return 0;
    }
    ((v * 255 + max / 2) / max) as u8
}

/// DIB → RGBA8（行优先、自上而下、无内边距）。
///
/// 返回 `(width, height, rgba)`。
pub fn dib_to_rgba(bytes: &[u8], info: &DibInfo) -> Option<(u32, u32, Vec<u8>)> {
    let w = info.width as usize;
    let h = info.height.unsigned_abs() as usize;
    let top_down = info.height < 0;
    let bpp = info.bit_count as usize / 8;
    // 每行按 4 字节对齐（DIB 的行步长规则）。
    let stride = (info.bit_count as usize * w).div_ceil(32) * 4;
    let needed = info.pixel_offset.checked_add(stride.checked_mul(h)?)?;
    if needed > bytes.len() {
        return None;
    }
    // 上限保护：拒绝超出设置层允许范围的超大图，避免一次性分配几百 MB。
    if w.checked_mul(h)? > 200_000_000 {
        return None;
    }

    let mut rgba = vec![0u8; w * h * 4];
    let mut any_alpha = false;

    for y in 0..h {
        let src_row = if top_down { y } else { h - 1 - y };
        let row = &bytes[info.pixel_offset + src_row * stride..][..w * bpp];
        let dst = &mut rgba[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let px = &row[x * bpp..];
            let (r, g, b, a) = if bpp == 4 {
                let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                (
                    extract(v, info.red_mask),
                    extract(v, info.green_mask),
                    extract(v, info.blue_mask),
                    extract(v, info.alpha_mask),
                )
            } else {
                let v = u32::from_le_bytes([px[0], px[1], px[2], 0]);
                (
                    extract(v, info.red_mask),
                    extract(v, info.green_mask),
                    extract(v, info.blue_mask),
                    255,
                )
            };
            any_alpha |= a != 0;
            let o = x * 4;
            dst[o] = r;
            dst[o + 1] = g;
            dst[o + 2] = b;
            dst[o + 3] = a;
        }
    }

    // 全 0 alpha 是 32 位 DIB 的常见写法（等价于"没有 alpha 信息"），当作不透明，
    // 否则粘贴出去会变成一张全透明的空图。
    if !any_alpha {
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }

    Some((w as u32, h as u32, rgba))
}

/// RGBA8 → `CF_DIBV5` 负载（124 字节头 + BI_BITFIELDS + 自下而上的 BGRA）。
pub fn rgba_to_dibv5(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let stride = w * 4;
    let mut out = vec![0u8; BITMAPV5HEADER_SIZE + stride * h];

    // BITMAPV5HEADER
    out[0..4].copy_from_slice(&(BITMAPV5HEADER_SIZE as u32).to_le_bytes());
    out[4..8].copy_from_slice(&(width as i32).to_le_bytes());
    out[8..12].copy_from_slice(&(height as i32).to_le_bytes()); // 正数 = 自下而上
    out[12..14].copy_from_slice(&1u16.to_le_bytes()); // biPlanes
    out[14..16].copy_from_slice(&32u16.to_le_bytes()); // biBitCount
    out[16..20].copy_from_slice(&BI_BITFIELDS.to_le_bytes());
    out[20..24].copy_from_slice(&((stride * h) as u32).to_le_bytes()); // biSizeImage
    out[24..28].copy_from_slice(&2835i32.to_le_bytes()); // 72 DPI
    out[28..32].copy_from_slice(&2835i32.to_le_bytes());
    out[32..36].copy_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out[36..40].copy_from_slice(&0u32.to_le_bytes()); // biClrImportant
    out[40..44].copy_from_slice(&0x00FF_0000u32.to_le_bytes()); // red mask
    out[44..48].copy_from_slice(&0x0000_FF00u32.to_le_bytes()); // green mask
    out[48..52].copy_from_slice(&0x0000_00FFu32.to_le_bytes()); // blue mask
    out[52..56].copy_from_slice(&0xFF00_0000u32.to_le_bytes()); // alpha mask
    out[56..60].copy_from_slice(&0x7352_4742u32.to_le_bytes()); // LCS_sRGB

    for y in 0..h {
        let src = &rgba[(h - 1 - y) * stride..(h - y) * stride];
        let dst = &mut out[BITMAPV5HEADER_SIZE + y * stride..][..stride];
        for x in 0..w {
            let s = &src[x * 4..];
            dst[x * 4] = s[2]; // B
            dst[x * 4 + 1] = s[1]; // G
            dst[x * 4 + 2] = s[0]; // R
            dst[x * 4 + 3] = s[3]; // A
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rgba(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                v.push((x * 17) as u8);
                v.push((y * 29) as u8);
                v.push(((x + y) * 11) as u8);
                v.push(255);
            }
        }
        v
    }

    #[test]
    fn dibv5_roundtrip_preserves_pixels() {
        let (w, h) = (7u32, 5u32);
        let rgba = sample_rgba(w, h);
        let dib = rgba_to_dibv5(w, h, &rgba).unwrap();
        assert_eq!(&dib[0..4], &(124u32).to_le_bytes());
        assert_eq!(rd_i32(&dib, 8), Some(5), "biHeight 必须为正（自下而上）");

        let info = parse_dib(&dib).unwrap();
        assert_eq!(info.width, 7);
        assert_eq!(info.height, 5);
        assert_eq!(info.pixel_offset, BITMAPV5HEADER_SIZE);
        let (rw, rh, back) = dib_to_rgba(&dib, &info).unwrap();
        assert_eq!((rw, rh), (w, h));
        assert_eq!(back, rgba, "往返必须逐像素一致（含垂直翻转）");
    }

    #[test]
    fn dibv5_roundtrip_with_alpha() {
        let (w, h) = (3u32, 2u32);
        let mut rgba = sample_rgba(w, h);
        rgba[3] = 0; // 让某个像素全透明
        rgba[7] = 128;
        let dib = rgba_to_dibv5(w, h, &rgba).unwrap();
        let info = parse_dib(&dib).unwrap();
        let (_, _, back) = dib_to_rgba(&dib, &info).unwrap();
        assert_eq!(back, rgba, "有非零 alpha 时必须原样保留透明通道");
    }

    #[test]
    fn top_down_24bpp_is_decoded() {
        // 手工构造 2x2 24bpp 自上而下（biHeight = -2）的 DIB。
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&40u32.to_le_bytes());
        b[4..8].copy_from_slice(&2i32.to_le_bytes());
        b[8..12].copy_from_slice(&(-2i32).to_le_bytes());
        b[12..14].copy_from_slice(&1u16.to_le_bytes());
        b[14..16].copy_from_slice(&24u16.to_le_bytes());
        // 第一行：红、绿；第二行：蓝、白（BGR 顺序，行按 4 字节对齐）
        b.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        b.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]);
        let info = parse_dib(&b).unwrap();
        assert_eq!(info.bit_count, 24);
        let (w, h, rgba) = dib_to_rgba(&b, &info).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(
            &rgba[0..4],
            &[255, 0, 0, 255],
            "红色，且 alpha 补齐为不透明"
        );
        assert_eq!(&rgba[4..8], &[0, 255, 0, 255], "绿色");
        assert_eq!(&rgba[8..12], &[0, 0, 255, 255], "蓝色（第二行第一列）");
    }

    #[test]
    fn all_zero_alpha_is_treated_as_opaque() {
        // 32 位 BI_RGB 且 alpha 全 0 —— 资源管理器/部分截图工具就是这个写法。
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&40u32.to_le_bytes());
        b[4..8].copy_from_slice(&1i32.to_le_bytes());
        b[8..12].copy_from_slice(&1i32.to_le_bytes());
        b[12..14].copy_from_slice(&1u16.to_le_bytes());
        b[14..16].copy_from_slice(&32u16.to_le_bytes());
        b.extend_from_slice(&[10, 20, 30, 0]);
        let info = parse_dib(&b).unwrap();
        let (_, _, rgba) = dib_to_rgba(&b, &info).unwrap();
        assert_eq!(rgba, vec![30, 20, 10, 255]);
    }

    #[test]
    fn zero_masks_fall_back_to_default_bgra() {
        // 截图工具常见的写法：V5 头（124 字节），但 RGB 掩码字段全 0。
        // 照掩码取值会得到全黑，必须回落到默认 BGRA 布局。
        let mut b = vec![0u8; BITMAPV5HEADER_SIZE];
        b[0..4].copy_from_slice(&(BITMAPV5HEADER_SIZE as u32).to_le_bytes());
        b[4..8].copy_from_slice(&2i32.to_le_bytes()); // 宽
        b[8..12].copy_from_slice(&1i32.to_le_bytes()); // 高
        b[14..16].copy_from_slice(&32u16.to_le_bytes()); // 32bpp
                                                         // biCompression = BI_RGB(0)、掩码 40..56 保持 0 —— 正是问题现场
        b.extend_from_slice(&[30, 20, 10, 0, 60, 50, 40, 0]);
        let info = parse_dib(&b).expect("应当能解析");
        assert_eq!(info.red_mask, 0x00FF_0000, "RGB 掩码必须回落");
        assert_eq!(info.alpha_mask, 0xFF00_0000);
        let (w, h, rgba) = dib_to_rgba(&b, &info).expect("应当能解码");
        assert_eq!((w, h), (2, 1));
        // 前两个像素是 (B,G,R)=(30,20,10)/(60,50,40) → RGB (10,20,30)/(40,50,60)
        assert_eq!(&rgba[0..4], &[10, 20, 30, 255]);
        assert_eq!(&rgba[4..8], &[40, 50, 60, 255]);
    }

    #[test]
    fn bitfields_masks_with_40_byte_header() {
        // 老约定：BITMAPINFOHEADER + 3 个掩码 DWORD，像素从 52 字节开始。
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&40u32.to_le_bytes());
        b[4..8].copy_from_slice(&1i32.to_le_bytes());
        b[8..12].copy_from_slice(&1i32.to_le_bytes());
        b[12..14].copy_from_slice(&1u16.to_le_bytes());
        b[14..16].copy_from_slice(&32u16.to_le_bytes());
        b[16..20].copy_from_slice(&BI_BITFIELDS.to_le_bytes());
        b.extend_from_slice(&0x00FF_0000u32.to_le_bytes());
        b.extend_from_slice(&0x0000_FF00u32.to_le_bytes());
        b.extend_from_slice(&0x0000_00FFu32.to_le_bytes());
        b.extend_from_slice(&[1, 2, 3, 0]);
        let info = parse_dib(&b).unwrap();
        assert_eq!(info.pixel_offset, 52);
        let (_, _, rgba) = dib_to_rgba(&b, &info).unwrap();
        assert_eq!(rgba, vec![3, 2, 1, 255]);
    }

    #[test]
    fn rejects_unsupported_and_truncated_input() {
        assert!(parse_dib(&[]).is_none());
        assert!(parse_dib(&[0u8; 39]).is_none());
        // 8 位索引色：不支持，直接拒绝而不是解出错色。
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&40u32.to_le_bytes());
        b[4..8].copy_from_slice(&4i32.to_le_bytes());
        b[8..12].copy_from_slice(&4i32.to_le_bytes());
        b[14..16].copy_from_slice(&8u16.to_le_bytes());
        assert!(parse_dib(&b).is_none());
        // 头部声称有像素但缓冲区不够。
        let mut c = vec![0u8; 40];
        c[0..4].copy_from_slice(&40u32.to_le_bytes());
        c[4..8].copy_from_slice(&64i32.to_le_bytes());
        c[8..12].copy_from_slice(&64i32.to_le_bytes());
        c[14..16].copy_from_slice(&32u16.to_le_bytes());
        let info = parse_dib(&c).unwrap();
        assert!(dib_to_rgba(&c, &info).is_none());
    }

    #[test]
    fn rgba_to_dibv5_validates_arguments() {
        assert!(rgba_to_dibv5(0, 1, &[0; 4]).is_none());
        assert!(rgba_to_dibv5(2, 2, &[0; 4]).is_none(), "缓冲区不足必须拒绝");
    }
}
