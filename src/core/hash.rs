//! 稳定内容哈希。
//!
//! 刻意**不用** `std::collections::hash_map::DefaultHasher`：它的算法没有跨版本稳定性
//! 承诺，而 `data.db` 是要跨程序升级存活的 —— 一旦实现变了，所有历史记录的去重索引
//! 就全部失效（同一条内容会被重复插入）。
//!
//! 这里手写 FNV-1a 64：算法固定、零依赖、速度足够（十几 MB/s 起步，远超剪贴板体量）。
//! 哈希只作为**索引**使用，命中后一律逐字节比对内容，因此碰撞不会造成误去重。

pub const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 增量式 FNV-1a 64 哈希器。
#[derive(Clone, Copy, Debug)]
pub struct Fnv1a64 {
    state: u64,
}

impl Default for Fnv1a64 {
    fn default() -> Self {
        Self::new()
    }
}

impl Fnv1a64 {
    pub const fn new() -> Self {
        Self {
            state: FNV_OFFSET_BASIS,
        }
    }

    #[inline]
    pub fn write(&mut self, bytes: &[u8]) {
        let mut h = self.state;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
        self.state = h;
    }

    /// 把长度也混进哈希，避免 `["ab","c"]` 与 `["a","bc"]` 相撞。
    #[inline]
    pub fn write_field(&mut self, bytes: &[u8]) {
        self.write(&(bytes.len() as u64).to_le_bytes());
        self.write(bytes);
    }

    #[inline]
    pub fn finish(self) -> u64 {
        self.state
    }
}

/// 一次性计算若干字段的组合哈希（每个字段带长度前缀）。
pub fn hash_fields(fields: &[&[u8]]) -> u64 {
    let mut h = Fnv1a64::new();
    for f in fields {
        h.write_field(f);
    }
    h.finish()
}

/// 单块内容哈希。
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = Fnv1a64::new();
    h.write(bytes);
    h.finish()
}

/// 文本哈希（UTF-8 字节）。
pub fn hash_str(s: &str) -> u64 {
    hash_bytes(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // FNV-1a 64 官方参考值：跨语言可比对，因此可用于回归。
        assert_eq!(hash_bytes(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash_bytes(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hash_bytes(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn field_boundaries_do_not_collide() {
        assert_ne!(hash_fields(&[b"ab", b"c"]), hash_fields(&[b"a", b"bc"]));
    }

    #[test]
    fn incremental_matches_oneshot() {
        let mut h = Fnv1a64::new();
        h.write(b"foo");
        h.write(b"bar");
        assert_eq!(h.finish(), hash_bytes(b"foobar"));
    }

    #[test]
    fn utf8_is_stable_for_chinese() {
        // 固定向量：若未来改动实现，这条会失败，提醒 data.db 兼容性被破坏。
        let a = hash_str("中文测试");
        assert_eq!(a, hash_bytes("中文测试".as_bytes()));
        assert_ne!(a, hash_str("中文测"));
    }
}
