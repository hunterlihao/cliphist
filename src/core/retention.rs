//! 保留策略：纯计算，不碰数据库。
//!
//! 把"该不该删"的判断从 SQL 里抽出来，是为了让边界（置顶豁免、0 = 不限、空库）
//! 能被单元测试直接覆盖，而不是靠跑一个真实库去猜。

/// 返回应当删除的时间下界（毫秒时间戳）。`keep_days <= 0` 表示不按时间清理。
pub fn cutoff_ms(keep_days: i64, now_ms: i64) -> Option<i64> {
    if keep_days <= 0 {
        return None;
    }
    let days_ms = keep_days.saturating_mul(24 * 60 * 60 * 1000);
    Some(now_ms.saturating_sub(days_ms))
}

/// 非置顶条数是否超预算。
pub fn over_item_budget(unpinned_count: i64, max_items: i64) -> bool {
    max_items > 0 && unpinned_count > max_items
}

/// 图片总量是否超预算（`max_image_bytes == 0` 视为不限制）。
pub fn over_image_budget(image_bytes: i64, max_image_bytes: i64) -> bool {
    max_image_bytes > 0 && image_bytes > max_image_bytes
}

/// 是否需要跑一次 `incremental_vacuum` 把空闲页还给系统。
///
/// 频繁 VACUUM 会让便携版磁盘写入放大，所以只在删除量累积到阈值时才做。
pub fn should_incremental_vacuum(deleted_since_last: usize, threshold: usize) -> bool {
    threshold > 0 && deleted_since_last >= threshold
}

/// 删除量的默认阈值：一次清理删掉 200 行以上才值得整理空闲页。
pub const VACUUM_ROW_THRESHOLD: usize = 200;

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    #[test]
    fn cutoff_respects_zero_means_unlimited() {
        assert_eq!(cutoff_ms(0, 1_000_000), None);
        assert_eq!(cutoff_ms(-3, 1_000_000), None);
    }

    #[test]
    fn cutoff_subtracts_days() {
        let now = 30 * DAY;
        assert_eq!(cutoff_ms(30, now), Some(0));
        assert_eq!(cutoff_ms(1, now), Some(now - DAY));
    }

    #[test]
    fn cutoff_saturates_instead_of_overflowing() {
        // 手改 data.db 塞进一个荒谬的天数时不能 panic、不能回绕。
        // i64::MAX 天 ≈ 2.5e14 年，days_ms 先饱和到 i64::MAX；
        // 再从 now 做饱和减法，于是结果被钳在 i64 的边界上。
        assert_eq!(cutoff_ms(i64::MAX, 0), Some(-(i64::MAX))); // = i64::MIN + 1
        assert_eq!(cutoff_ms(i64::MAX, i64::MIN), Some(i64::MIN));
        // 正常范围不受影响
        assert_eq!(cutoff_ms(30, 30 * DAY), Some(0));
    }

    #[test]
    fn item_budget_boundaries() {
        assert!(!over_item_budget(500, 500));
        assert!(over_item_budget(501, 500));
        // 0 = 不按条数限制
        assert!(!over_item_budget(1_000_000, 0));
    }

    #[test]
    fn image_budget_boundaries() {
        assert!(!over_image_budget(100, 100));
        assert!(over_image_budget(101, 100));
        assert!(!over_image_budget(1 << 40, 0));
    }

    #[test]
    fn vacuum_threshold() {
        assert!(!should_incremental_vacuum(199, VACUUM_ROW_THRESHOLD));
        assert!(should_incremental_vacuum(200, VACUUM_ROW_THRESHOLD));
    }
}
