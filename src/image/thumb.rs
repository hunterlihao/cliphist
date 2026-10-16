//! 位图缩放与 PNG 编解码。
//!
//! 缩放用面积平均（box filter）而不是最近邻：缩略图是给人看的，最近邻在截图里的
//! 文字边上会产生难以忍受的锯齿。只用整数运算，不需要浮点。

use png::{BitDepth, ColorType, Decoder, Encoder};

use crate::core::model::ImageData;

/// 缩略图长边上限（像素）。128 在 40px 的行内显示区里已有 2 倍余量。
pub const THUMB_MAX_EDGE: u32 = 128;

/// 是否需要降采样。
pub fn needs_downscale(w: u32, h: u32, max_edge: u32) -> bool {
    max_edge > 0 && w.max(h) > max_edge
}

/// 面积平均降采样。`max_edge == 0` 或本来就够小时返回原图副本。
pub fn downscale_rgba(src: &ImageData, max_edge: u32) -> ImageData {
    if !needs_downscale(src.width, src.height, max_edge) {
        return src.clone();
    }
    let scale = max_edge as f64 / src.width.max(src.height) as f64;
    let dw = ((src.width as f64 * scale).round() as u32).max(1);
    let dh = ((src.height as f64 * scale).round() as u32).max(1);
    let mut dst = vec![0u8; (dw * dh * 4) as usize];

    for dy in 0..dh {
        let y0 = dy as u64 * src.height as u64 / dh as u64;
        let y1 = (((dy + 1) as u64 * src.height as u64) / dh as u64).max(y0 + 1);
        for dx in 0..dw {
            let x0 = dx as u64 * src.width as u64 / dw as u64;
            let x1 = (((dx + 1) as u64 * src.width as u64) / dw as u64).max(x0 + 1);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for y in y0..y1.min(src.height as u64) {
                for x in x0..x1.min(src.width as u64) {
                    let o = ((y as u32 * src.width + x as u32) * 4) as usize;
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += src.rgba[o + c] as u32;
                    }
                    n += 1;
                }
            }
            let o = ((dy * dw + dx) * 4) as usize;
            if n == 0 {
                continue;
            }
            for (c, a) in acc.iter().enumerate() {
                dst[o + c] = ((a + n / 2) / n) as u8;
            }
        }
    }

    ImageData {
        width: dw,
        height: dh,
        rgba: dst,
    }
}

/// RGBA8 → PNG 字节。图片入库时调用（在写线程上，不阻塞 UI）。
pub fn encode_png_rgba(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 {
        return Err("图片尺寸为 0".into());
    }
    if rgba.len() < (w as usize * h as usize * 4) {
        return Err("像素缓冲区不足".into());
    }
    let mut out = Vec::new();
    {
        let mut enc = Encoder::new(&mut out, w, h);
        enc.set_color(ColorType::Rgba);
        enc.set_depth(BitDepth::Eight);
        // 截图这类大色块图像，默认压缩级别足够；调高会明显拖慢写线程。
        enc.set_compression(png::Compression::Default);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(&rgba[..(w as usize * h as usize * 4)])
            .map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// PNG 字节 → RGBA8。只接受 8 位 RGB/RGBA（本程序自己写出的就是 RGBA）。
pub fn decode_png_rgba(bytes: &[u8]) -> Result<ImageData, String> {
    let dec = Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let w = info.width;
    let h = info.height;
    let data = &buf[..info.buffer_size()];

    let rgba = match (info.color_type, info.bit_depth) {
        (ColorType::Rgba, BitDepth::Eight) => data.to_vec(),
        (ColorType::Rgb, BitDepth::Eight) => {
            let mut v = Vec::with_capacity(w as usize * h as usize * 4);
            for px in data.chunks_exact(3) {
                v.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            v
        }
        (ct, bd) => return Err(format!("不支持的 PNG 格式：{ct:?}/{bd:?}")),
    };

    if rgba.len() < (w as usize * h as usize * 4) {
        return Err("PNG 数据不完整".into());
    }
    Ok(ImageData {
        width: w,
        height: h,
        rgba,
    })
}

/// 生成缩略图 PNG（长边 128），失败返回 `None`（调用方会把 `thumb_state` 标为 2）。
pub fn make_thumbnail_png(src: &ImageData) -> Option<Vec<u8>> {
    if src.width == 0 || src.height == 0 {
        return None;
    }
    let small = downscale_rgba(src, THUMB_MAX_EDGE);
    encode_png_rgba(small.width, small.height, &small.rgba).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> ImageData {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.push((x % 256) as u8);
                rgba.push((y % 256) as u8);
                rgba.push(((x * y) % 256) as u8);
                rgba.push(255);
            }
        }
        ImageData {
            width: w,
            height: h,
            rgba,
        }
    }

    #[test]
    fn png_roundtrip_is_lossless() {
        let src = img(37, 21);
        let png = encode_png_rgba(src.width, src.height, &src.rgba).unwrap();
        assert_eq!(&png[1..4], b"PNG", "必须是合法 PNG 魔数");
        let back = decode_png_rgba(&png).unwrap();
        assert_eq!((back.width, back.height), (src.width, src.height));
        assert_eq!(back.rgba, src.rgba);
    }

    #[test]
    fn png_rejects_garbage() {
        assert!(decode_png_rgba(b"not a png").is_err());
        assert!(encode_png_rgba(0, 10, &[]).is_err());
        assert!(encode_png_rgba(4, 4, &[0; 8]).is_err());
    }

    #[test]
    fn downscale_shrinks_long_edge_and_keeps_aspect() {
        let src = img(1000, 500);
        let small = downscale_rgba(&src, 128);
        assert_eq!(small.width, 128);
        assert_eq!(small.height, 64);
        assert_eq!(small.rgba.len(), 128 * 64 * 4);

        // 竖图走另一条分支
        let tall = img(300, 900);
        let small = downscale_rgba(&tall, 128);
        assert_eq!(small.height, 128);
        assert_eq!(small.width, 43);
    }

    #[test]
    fn downscale_never_upscales() {
        let src = img(10, 10);
        let same = downscale_rgba(&src, 128);
        assert_eq!((same.width, same.height), (10, 10));
        assert_eq!(same.rgba, src.rgba);
    }

    #[test]
    fn downscale_averages_block_colors() {
        // 4x4 图：左半黑右半白 → 缩到 2x2 时每个目标格正好覆盖同色块。
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for y in 0..4usize {
            for x in 0..4usize {
                let v = if x < 2 { 0 } else { 255 };
                let o = (y * 4 + x) * 4;
                rgba[o] = v;
                rgba[o + 1] = v;
                rgba[o + 2] = v;
                rgba[o + 3] = 255;
            }
        }
        let src = ImageData {
            width: 4,
            height: 4,
            rgba,
        };
        let small = downscale_rgba(&src, 2);
        assert_eq!((small.width, small.height), (2, 2));
        assert_eq!(small.rgba[0], 0, "左格覆盖纯黑块");
        assert_eq!(small.rgba[4], 255, "右格覆盖纯白块");

        // 真正验证"平均"：2x1（黑+白）→ 1x1 必须是 128 而不是任取一边。
        let mut half = vec![0u8, 0, 0, 255];
        half.extend_from_slice(&[255, 255, 255, 255]);
        let src = ImageData {
            width: 2,
            height: 1,
            rgba: half,
        };
        let one = downscale_rgba(&src, 1);
        assert_eq!((one.width, one.height), (1, 1));
        assert_eq!(one.rgba[0], 128, "非整块的边界必须做平均，不能取最近邻");

        // 3x1（0,255,255）→ 1x1 = 170：验证整数四舍五入。
        let mut three = vec![0u8, 0, 0, 255];
        three.extend_from_slice(&[255, 255, 255, 255]);
        three.extend_from_slice(&[255, 255, 255, 255]);
        let src = ImageData {
            width: 3,
            height: 1,
            rgba: three,
        };
        let one = downscale_rgba(&src, 1);
        assert_eq!(one.rgba[0], 170, "(0+255+255+1)/3 = 170");
    }

    #[test]
    fn thumbnail_is_small_and_decodable() {
        let big = img(1920, 1080);
        let thumb = make_thumbnail_png(&big).unwrap();
        let back = decode_png_rgba(&thumb).unwrap();
        assert_eq!(back.width, 128);
        assert_eq!(back.height, 72);
        // 缩略图必须远小于原图（否则存进 DB 就没意义了）。
        assert!(thumb.len() < 40_000, "缩略图 {} 字节偏大", thumb.len());
    }

    #[test]
    fn thumbnail_of_degenerate_image_is_none() {
        let empty = ImageData {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
        assert!(make_thumbnail_png(&empty).is_none());
    }
}
