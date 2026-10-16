//! 时间显示：相对时间与本地日期。
//!
//! 时间戳统一用 unix 毫秒（与 `items.created_at` 一致）。相对时间用纯整数运算，
//! 可以脱离系统时钟测试；只有"绝对日期"需要问系统要本地时区。

use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
use windows_sys::Win32::System::Time::FileTimeToSystemTime;

const MINUTE: i64 = 60 * 1000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// 相对时间：刚刚 / N 分钟前 / N 小时前 / 昨天 / N 天前 / 本地日期。
pub fn relative(now_ms: i64, then_ms: i64) -> String {
    let d = now_ms - then_ms;
    if d < 0 {
        // 时钟被回拨或数据库里有未来时间：不要显示"-3 分钟前"。
        return "刚刚".to_string();
    }
    if d < MINUTE {
        "刚刚".to_string()
    } else if d < HOUR {
        format!("{} 分钟前", d / MINUTE)
    } else if d < DAY {
        format!("{} 小时前", d / HOUR)
    } else if d < 2 * DAY {
        "昨天".to_string()
    } else if d < 30 * DAY {
        format!("{} 天前", d / DAY)
    } else {
        local_date(then_ms)
    }
}

/// 把 unix 毫秒转成本地日期文本（`2026年10月17日`）。
///
/// 走 FILETIME 而不是自己算儒略日：unix 毫秒是 UTC，直接算日期在 UTC+8 的
/// 凌晨会显示成前一天 —— 这类"差一天"的 bug 很难被发现，交给系统最省事。
pub fn local_date(unix_ms: i64) -> String {
    // 1601-01-01 到 1970-01-01 的 100ns 数
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    let ft_ticks = unix_ms.saturating_mul(10_000).saturating_add(EPOCH_DIFF);
    let file_time = FILETIME {
        dwLowDateTime: (ft_ticks as u64 & 0xFFFF_FFFF) as u32,
        dwHighDateTime: ((ft_ticks as u64 >> 32) & 0xFFFF_FFFF) as u32,
    };

    let mut local = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        FileTimeToLocalFileTime(&file_time, &mut local) != 0
            && FileTimeToSystemTime(&local, &mut st) != 0
    };
    if !ok {
        return "未知时间".to_string();
    }
    format!("{}年{}月{}日", st.wYear, st.wMonth, st.wDay)
}

/// 更短的日期（列表里用的紧凑形式：`10月14日`；跨年才带年份）。
pub fn local_date_short(unix_ms: i64, now_ms: i64) -> String {
    let full = local_date(unix_ms);
    let now = local_date(now_ms);
    // 同一年只留月日
    let year_now = now.split('年').next().unwrap_or("");
    if let Some(rest) = full.strip_prefix(&format!("{year_now}年")) {
        rest.to_string()
    } else {
        full
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_buckets() {
        let now = 100 * DAY;
        assert_eq!(relative(now, now), "刚刚");
        assert_eq!(relative(now, now - 30_000), "刚刚");
        assert_eq!(relative(now, now - MINUTE), "1 分钟前");
        assert_eq!(relative(now, now - 59 * MINUTE), "59 分钟前");
        assert_eq!(relative(now, now - HOUR), "1 小时前");
        assert_eq!(relative(now, now - 23 * HOUR), "23 小时前");
        assert_eq!(relative(now, now - DAY), "昨天");
        assert_eq!(relative(now, now - 3 * DAY), "3 天前");
    }

    #[test]
    fn future_timestamps_do_not_produce_negative_text() {
        let now = 50 * DAY;
        assert_eq!(relative(now, now + 5 * MINUTE), "刚刚");
    }

    #[test]
    fn old_timestamps_fall_back_to_calendar_date() {
        let now = 400 * DAY;
        let then = now - 40 * DAY;
        let s = relative(now, then);
        assert!(s.contains('年') && s.contains('月'), "实际得到 {s}");
    }

    #[test]
    fn local_date_of_epoch_is_1970_in_utc_range() {
        // 1970-01-01 00:00 UTC 在东八区是 08:00，仍是 1 月 1 日；
        // 在 UTC-5 是 1969-12-31 19:00 → 12 月 31 日。两种都合法，只验证形状。
        let s = local_date(0);
        assert!(s.ends_with('日'), "实际得到 {s}");
        assert!(s.contains('年'));
    }

    #[test]
    fn short_date_drops_current_year() {
        let now = 1_700_000_000_000; // 2023-11
        let same_year = now - DAY;
        let s = local_date_short(same_year, now);
        assert!(!s.contains('年'), "同年不应带年份：{s}");
        let s2 = local_date_short(now - 800 * DAY, now);
        assert!(s2.contains('年'), "跨年必须带年份：{s2}");
    }
}
