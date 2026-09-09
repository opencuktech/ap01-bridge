//! 不依赖时钟和时区的公历时间格式化。

/// 把 Unix epoch 秒格式化为 UTC 日期时间，年份至少四位。
///
/// 超过公历 9999 年时保留完整年份，避免截断或溢出；这类扩展年份超出 RFC 3339 范围。
pub fn format_rfc3339_utc(epoch_secs: u64) -> String {
    // 以三月为年首，把闰日放在年末；每四百年恰好有 146097 天。
    let days = epoch_secs / 86_400 + 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    year += u64::from(month <= 2);
    let hour = epoch_secs % 86_400 / 3_600;
    let minute = epoch_secs % 3_600 / 60;
    let second = epoch_secs % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates_and_calendar_boundaries() {
        for (seconds, expected) in [
            (0, "1970-01-01T00:00:00Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_709_164_800, "2024-02-29T00:00:00Z"),
            (4_107_456_000, "2100-02-28T00:00:00Z"),
            (4_107_542_399, "2100-02-28T23:59:59Z"),
            (4_107_542_400, "2100-03-01T00:00:00Z"),
            (1_735_689_599, "2024-12-31T23:59:59Z"),
            (1_735_689_600, "2025-01-01T00:00:00Z"),
            (1_714_521_599, "2024-04-30T23:59:59Z"),
            (1_714_521_600, "2024-05-01T00:00:00Z"),
            (1_788_668_411, "2026-09-06T04:20:11Z"),
            (253_402_300_799, "9999-12-31T23:59:59Z"),
            (u64::MAX, "584554051223-11-09T07:00:15Z"),
        ] {
            assert_eq!(format_rfc3339_utc(seconds), expected, "时间戳：{seconds}");
        }
    }
}
