//! 剪贴板负载的纯编解码：`DROPFILES` 与 `Preferred DropEffect`。
//!
//! 这里是"文件复制"能被粘回资源管理器的关键。Win32 侧只负责 `GlobalAlloc` + 拷贝，
//! 字节布局全部在这里生成与解析，因此可以脱离桌面环境做往返测试。

use crate::core::hash::{hash_fields, Fnv1a64};
use crate::core::model::PathEntry;

/// `DROPFILES` 结构体大小（`DWORD pFiles; POINT pt; BOOL fNC; BOOL fWide;`，4 字节对齐）。
pub const DROPFILES_SIZE: usize = 20;

/// 构造 `CF_HDROP` 负载：`DROPFILES` 头 + UTF-16LE 路径列表（每条以 NUL 结尾，最后再加一个 NUL）。
pub fn build_hdrop(paths: &[String]) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::with_capacity(paths.iter().map(|p| p.len() * 2 + 2).sum());
    for p in paths {
        for unit in p.encode_utf16().chain(std::iter::once(0)) {
            body.extend_from_slice(&unit.to_le_bytes());
        }
    }
    if body.is_empty() {
        // 空列表也必须留下一个终止 NUL，否则某些 shell 会一直往后读。
        body.extend_from_slice(&[0, 0]);
    } else {
        body.extend_from_slice(&[0, 0]);
    }

    let mut out = Vec::with_capacity(DROPFILES_SIZE + body.len());
    out.extend_from_slice(&(DROPFILES_SIZE as u32).to_le_bytes()); // pFiles
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    out.extend_from_slice(&0i32.to_le_bytes()); // fNC = FALSE
    out.extend_from_slice(&1i32.to_le_bytes()); // fWide = TRUE
    out.extend_from_slice(&body);
    out
}

/// 解析 `CF_HDROP` 负载（`build_hdrop` 的逆运算，同时兼容 ANSI 变体）。
pub fn parse_hdrop(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.len() < DROPFILES_SIZE {
        return None;
    }
    let p_files = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
    let f_wide = i32::from_le_bytes(bytes[16..20].try_into().ok()?) != 0;
    if p_files < DROPFILES_SIZE || p_files > bytes.len() {
        return None;
    }
    let body = &bytes[p_files..];
    let mut out = Vec::new();

    if f_wide {
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let mut cur: Vec<u16> = Vec::new();
        for u in units {
            if u == 0 {
                if cur.is_empty() {
                    break; // 列表终止
                }
                out.push(String::from_utf16_lossy(&cur));
                cur.clear();
            } else {
                cur.push(u);
            }
        }
        if !cur.is_empty() {
            out.push(String::from_utf16_lossy(&cur));
        }
    } else {
        for chunk in body.split(|&b| b == 0) {
            if chunk.is_empty() {
                break;
            }
            // 系统剪贴板里的 ANSI 路径按当前 ANSI 代码页解释；这里用有损 UTF-8 兜底
            // （只影响极端老程序的粘贴来源，不影响我们自己生成的内容）。
            out.push(String::from_utf8_lossy(chunk).into_owned());
        }
    }
    Some(out)
}

/// `Preferred DropEffect` 的负载：一个小端 DWORD。
pub fn build_drop_effect(effect: u32) -> [u8; 4] {
    effect.to_le_bytes()
}

pub fn parse_drop_effect(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes(bytes[0..4].try_into().ok()?))
}

/// `DROPEFFECT_COPY`：粘贴到目标文件夹时**复制**而不是移走原文件。
pub const DROPEFFECT_COPY: u32 = 1;
/// `DROPEFFECT_MOVE`：采集时用于给"剪切"来源打标记。
pub const DROPEFFECT_MOVE: u32 = 2;

/// 文件列表的展示文本：路径按行拼接（同时作为全文搜索的正文）。
pub fn join_paths_text(paths: &[String]) -> String {
    paths.join("\r\n")
}

/// 把 CRLF 文本拆回路径列表。
pub fn split_paths_text(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// 文件条目的内容哈希：把路径序列（含目录标记）整体参与哈希。
pub fn hash_paths(entries: &[PathEntry]) -> u64 {
    let mut h = Fnv1a64::new();
    for e in entries {
        h.write(&[u8::from(e.is_dir)]);
        h.write_field(e.path.as_bytes());
    }
    h.finish()
}

/// 文本与图片哈希共用的入口（图片哈希基于缩放前的 RGBA 像素）。
pub fn hash_image(w: u32, h: u32, rgba: &[u8]) -> u64 {
    hash_fields(&[
        &w.to_le_bytes(),
        &h.to_le_bytes(),
        &(rgba.len() as u64).to_le_bytes(),
        rgba,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Vec<String> {
        vec![
            r"C:\Users\me\文档\报告 v2.txt".to_string(),
            r"\\?\D:\很长的路径\子目录\图片.png".to_string(),
        ]
    }

    #[test]
    fn hdrop_roundtrip_with_unicode() {
        let bytes = build_hdrop(&paths());
        assert_eq!(&bytes[0..4], &(DROPFILES_SIZE as u32).to_le_bytes());
        assert_eq!(&bytes[16..20], &1i32.to_le_bytes(), "fWide 必须为 TRUE");
        assert_eq!(parse_hdrop(&bytes).unwrap(), paths());
    }

    #[test]
    fn hdrop_empty_list_is_still_terminated() {
        let bytes = build_hdrop(&[]);
        assert_eq!(bytes.len(), DROPFILES_SIZE + 2);
        assert_eq!(parse_hdrop(&bytes).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn hdrop_rejects_truncated_input() {
        assert!(parse_hdrop(&[]).is_none());
        assert!(parse_hdrop(&[0u8; 8]).is_none());
        let mut bad = build_hdrop(&paths());
        bad[0] = 0xFF; // pFiles 指向越界
        assert!(parse_hdrop(&bad).is_none());
    }

    #[test]
    fn drop_effect_roundtrip() {
        assert_eq!(
            parse_drop_effect(&build_drop_effect(DROPEFFECT_COPY)),
            Some(DROPEFFECT_COPY)
        );
        assert_eq!(
            parse_drop_effect(&build_drop_effect(DROPEFFECT_MOVE)),
            Some(DROPEFFECT_MOVE)
        );
        assert_eq!(parse_drop_effect(&[1, 0]), None);
    }

    #[test]
    fn paths_text_roundtrip() {
        let p = paths();
        assert_eq!(split_paths_text(&join_paths_text(&p)), p);
        assert_eq!(split_paths_text(""), Vec::<String>::new());
    }

    #[test]
    fn hash_paths_is_order_and_kind_sensitive() {
        let a = vec![PathEntry {
            path: "a".into(),
            is_dir: false,
        }];
        let b = vec![PathEntry {
            path: "a".into(),
            is_dir: true,
        }];
        let c = vec![
            PathEntry {
                path: "b".into(),
                is_dir: false,
            },
            PathEntry {
                path: "a".into(),
                is_dir: false,
            },
        ];
        let d = vec![
            PathEntry {
                path: "a".into(),
                is_dir: false,
            },
            PathEntry {
                path: "b".into(),
                is_dir: false,
            },
        ];
        assert_ne!(hash_paths(&a), hash_paths(&b));
        assert_ne!(hash_paths(&c), hash_paths(&d));
        assert_eq!(hash_paths(&a), hash_paths(&a));
    }

    #[test]
    fn hash_image_depends_on_dimensions() {
        let px = vec![0u8; 16];
        assert_ne!(hash_image(2, 2, &px), hash_image(4, 1, &px));
        assert_eq!(hash_image(2, 2, &px), hash_image(2, 2, &px));
    }
}
