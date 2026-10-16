//! 预览文本生成与搜索匹配（纯逻辑）。
//!
//! 设计取舍：列表只加载 `items.preview`（≤200 字符），所以**搜索默认只在预览上做**。
//! 这换来"内存与历史条数解耦"，代价是长文本中间的内容搜不到 —— 因此提供
//! [`SearchPlan`]：预览无命中时才回退到 `text` 全文 LIKE，且限定在最近若干条、
//! 单条长度上限以内，保证最坏情况可控。

/// 生成单行预览：折叠所有空白（含换行/制表）为单空格，按**字符**（非字节）截断。
pub fn make_text_preview(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(max_chars.min(256) + 4);
    let mut pending_space = false;
    let mut chars = 0usize;
    let mut truncated = false;

    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        // 控制字符（除空白外）不参与预览，避免破坏单行绘制。
        if ch.is_control() {
            continue;
        }
        if pending_space {
            if chars + 1 >= max_chars {
                truncated = true;
                break;
            }
            out.push(' ');
            chars += 1;
            pending_space = false;
        }
        if chars >= max_chars {
            truncated = true;
            break;
        }
        out.push(ch);
        chars += 1;
    }

    if truncated {
        out.push('…');
    }
    if out.is_empty() && !text.is_empty() {
        // 全是空白/控制字符：给个可读占位，避免列表出现空行。
        out.push_str("(空白内容)");
    }
    out
}

/// 文件条目预览：`a.txt`、`a.txt、b.txt`、`a.txt 等 3 项`。
pub fn make_files_preview(files: &[crate::core::model::PathEntry]) -> String {
    let names: Vec<String> = files
        .iter()
        .map(|f| {
            let base = file_name_of(&f.path);
            if f.is_dir {
                format!("{base}\\")
            } else {
                base
            }
        })
        .collect();

    match names.len() {
        0 => "文件".to_string(),
        1 => names[0].clone(),
        2 => format!("{}、{}", names[0], names[1]),
        n => format!("{}、{} 等 {} 项", names[0], names[1], n),
    }
}

/// 图片预览。
pub fn make_image_preview(w: u32, h: u32) -> String {
    if w == 0 || h == 0 {
        "图片".to_string()
    } else {
        format!("图片 {w}×{h}")
    }
}

/// 从完整路径取最后一段（同时兼容 `/` 与 `\`，兼容 `\\?\` 前缀）。
pub fn file_name_of(path: &str) -> String {
    let trimmed = path.trim_end_matches(['\\', '/']);
    let cut = trimmed
        .rfind(['\\', '/'])
        .map(|i| i + 1)
        .unwrap_or_default();
    let name = &trimmed[cut..];
    if name.is_empty() {
        // 盘符根，如 `C:\`
        trimmed.to_string()
    } else {
        name.to_string()
    }
}

/// LIKE 通配符转义（配合 `ESCAPE '\'` 使用），并 trim 查询串。
pub fn escape_like(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 8);
    for ch in query.trim().chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// 内存内匹配（大小写不敏感；中文按子串匹配，不做分词）。
pub fn matches(preview: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    preview.to_lowercase().contains(&q.to_lowercase())
}

/// 最近若干条里做全文回退搜索时的参数。
#[derive(Clone, Copy, Debug)]
pub struct SearchPlan {
    /// 是否启用 `text` 全文回退。
    pub fallback_to_text: bool,
    /// 回退搜索只看最近多少条。
    pub fallback_scan_limit: i64,
    /// 回退搜索忽略超过此长度的正文（避免为了搜一条 2MB 的正文扫描全库）。
    pub fallback_max_text_len: i64,
}

impl Default for SearchPlan {
    fn default() -> Self {
        Self {
            fallback_to_text: true,
            fallback_scan_limit: 2000,
            fallback_max_text_len: 256 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::PathEntry;

    #[test]
    fn preview_collapses_whitespace() {
        assert_eq!(make_text_preview("a\n\tb   c\r\n", 50), "a b c");
    }

    #[test]
    fn preview_truncates_on_char_boundary() {
        let s = "中文中文中文";
        let p = make_text_preview(s, 4);
        assert_eq!(p, "中文中文…");
        // 关键：不能在多字节字符中间切断（否则会 panic / 出现乱码）。
        assert!(p.chars().count() == 5);
    }

    #[test]
    fn preview_handles_blank_only() {
        assert_eq!(make_text_preview("   \n\t ", 50), "(空白内容)");
        assert_eq!(make_text_preview("", 50), "");
    }

    #[test]
    fn preview_strips_control_chars() {
        assert_eq!(make_text_preview("a\u{0}b\u{7}c", 50), "abc");
    }

    #[test]
    fn files_preview_shapes() {
        let f = |p: &str, d: bool| PathEntry {
            path: p.to_string(),
            is_dir: d,
        };
        assert_eq!(make_files_preview(&[]), "文件");
        assert_eq!(make_files_preview(&[f(r"C:\a\one.txt", false)]), "one.txt");
        assert_eq!(
            make_files_preview(&[f(r"C:\a\one.txt", false), f(r"D:\b\two", true)]),
            "one.txt、two\\"
        );
        assert_eq!(
            make_files_preview(&[
                f(r"C:\a\one.txt", false),
                f(r"C:\a\two.txt", false),
                f(r"C:\a\three.txt", false)
            ]),
            "one.txt、two.txt 等 3 项"
        );
    }

    #[test]
    fn file_name_handles_prefix_and_root() {
        assert_eq!(file_name_of(r"\\?\C:\very\long\name.txt"), "name.txt");
        assert_eq!(file_name_of(r"C:\dir\"), "dir");
        assert_eq!(file_name_of(r"C:\"), "C:");
    }

    #[test]
    fn like_escaping() {
        assert_eq!(escape_like(" 100%_a\\b "), "100\\%\\_a\\\\b");
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(matches("Hello World", "hello"));
        assert!(matches("你好世界", "世界"));
        assert!(!matches("abc", "xyz"));
        assert!(matches("abc", "   "));
    }
}
