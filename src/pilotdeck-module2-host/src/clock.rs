//! 桥接层需要的时钟：ISO-8601 UTC（秒级）。
//!
//! 为什么自己实现：agent-host 只依赖 std + serde_json；`startedAt`/`completedAt`
//! 是 sidecar 契约要求的字符串字段，不能缺省（缺省会退化成 boundary-only）。
//! 精度只到秒，且明确是**桥接层观测时间**，不是 Kernel 事件时间（后者由事件头记录）。

use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 UTC 时间，形如 `2026-10-27T12:34:56Z`。
pub fn now_iso8601() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format_epoch_seconds(seconds)
}

/// 把 Unix 秒格式化为 ISO-8601（UTC，秒级）。
pub fn format_epoch_seconds(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rem = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant 的 days-from-civil 逆变换（1970-01-01 为 0）。
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::format_epoch_seconds;

    #[test]
    fn formats_known_epoch_seconds() {
        assert_eq!(format_epoch_seconds(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_epoch_seconds(1_000_000_000), "2001-09-09T01:46:40Z");
        // 闰年 2 月 29 日
        assert_eq!(format_epoch_seconds(951_782_400), "2000-02-29T00:00:00Z");
    }
}
