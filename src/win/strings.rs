//! 宽字符串与 Win32 字符串缓冲的转换helper。

/// `&str` → 以 NUL 结尾的 UTF-16 缓冲（Win32 `PCWSTR` 参数直接用 `.as_ptr()`）。
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 定长 `[u16; N]` 字段（如 `szTip`、`szInfo`）的填充：截断到 `N-1` 并补 NUL。
pub fn copy_fixed(dst: &mut [u16], s: &str) {
    if dst.is_empty() {
        return;
    }
    let n = dst.len() - 1;
    let mut i = 0;
    for u in s.encode_utf16() {
        if i >= n {
            break;
        }
        dst[i] = u;
        i += 1;
    }
    dst[i] = 0;
    // 其余位置清零，避免残留上一次的内容（结构体被复用时很常见）。
    for slot in &mut dst[i + 1..] {
        *slot = 0;
    }
}

/// 从可能没有 NUL 结尾的宽缓冲里取出字符串。
pub fn from_wide_until_nul(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// `Vec<u16>`（定长数组转来）→ String，按 NUL 截断。
pub fn from_fixed(dst: &[u16]) -> String {
    from_wide_until_nul(dst)
}

/// UTF-16 字节长度（用于 `RegSetValueExW` 的 cbData，含结尾 NUL）。
pub fn wide_byte_len(s: &str) -> u32 {
    ((s.encode_utf16().count() + 1) * 2) as u32
}

/// 把 `&[u16]` 视作字节切片（注册表写入用）。
pub fn wide_as_bytes(v: &[u16]) -> &[u8] {
    // SAFETY: u16 切片天然 2 字节对齐，按字节视图读取是安全的。
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_wide_appends_nul() {
        let w = to_wide("中文");
        assert_eq!(w.len(), 3);
        assert_eq!(w[2], 0);
        assert_eq!(from_wide_until_nul(&w), "中文");
    }

    #[test]
    fn copy_fixed_truncates_and_terminates() {
        let mut buf = [0xFFFFu16; 8];
        copy_fixed(&mut buf, "abcdefghij");
        assert_eq!(from_fixed(&buf), "abcdefg");
        assert_eq!(buf[7], 0);
    }

    #[test]
    fn copy_fixed_clears_previous_content() {
        let mut buf = [0u16; 16];
        copy_fixed(&mut buf, "long-content-here");
        copy_fixed(&mut buf, "x");
        assert_eq!(from_fixed(&buf), "x");
        assert_eq!(buf[2], 0);
    }

    #[test]
    fn from_fixed_without_nul() {
        assert_eq!(from_fixed(&[0x4E2Du16, 0x6587]), "中文");
    }

    #[test]
    fn byte_len_counts_nul() {
        assert_eq!(wide_byte_len("ab"), 6);
        assert_eq!(wide_byte_len(""), 2);
    }
}
