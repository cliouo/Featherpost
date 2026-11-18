//! 系统本地时间拆解辅助（零外部时区库依赖）。
//!
//! Unix 上直接调用 POSIX `tzset` + `localtime_r` 读取系统时区与夏令时偏移（`tm_gmtoff`）；
//! 非 Unix 平台返回 `None`，由调用方回退到固定 UTC 偏移或 `+0000`。

/// 本地日历时间拆解结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalDateTime {
    pub year: i32,
    /// `1..=12`
    pub month: u8,
    /// `1..=31`
    pub day: u8,
    /// `0..=6`（`0` = 周日 .. `6` = 周六）
    pub weekday: u8,
    /// `0..=23`
    pub hour: u8,
    /// `0..=59`
    pub minute: u8,
    /// `0..=60`（兼容闰秒）
    pub second: u8,
    /// 相对 UTC 的分钟偏移（如东八区为 `480`，北美中部冬令时为 `-360`）。
    pub utc_offset_minutes: i32,
}

#[cfg(unix)]
extern "C" {
    fn tzset();
}

/// 刷新进程对系统时区（含 `TZ` 环境变量）的缓存。
#[cfg(unix)]
pub fn refresh_timezone() {
    unsafe {
        tzset();
    }
}

/// 将 Unix 秒拆解为当前系统时区下的本地时间。超出 `time_t` / `tm` 可表达范围或非 Unix 时返回 `None`。
#[cfg(unix)]
pub fn local_parts(unix_secs: u64) -> Option<LocalDateTime> {
    let _ = i64::try_from(unix_secs).ok()?;
    let secs = libc::time_t::try_from(unix_secs).ok()?;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ptr = unsafe {
        tzset();
        libc::localtime_r(&secs, &mut tm)
    };
    if ptr.is_null() {
        return None;
    }
    let year = tm.tm_year.checked_add(1900)?;
    let month = tm
        .tm_mon
        .checked_add(1)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| (1..=12).contains(value))?;
    let day = u8::try_from(tm.tm_mday)
        .ok()
        .filter(|value| (1..=31).contains(value))?;
    let weekday = u8::try_from(tm.tm_wday)
        .ok()
        .filter(|value| *value <= 6)?;
    let hour = u8::try_from(tm.tm_hour)
        .ok()
        .filter(|value| *value <= 23)?;
    let minute = u8::try_from(tm.tm_min)
        .ok()
        .filter(|value| *value <= 59)?;
    let second = u8::try_from(tm.tm_sec)
        .ok()
        .filter(|value| *value <= 60)?;
    let utc_offset_minutes = i32::try_from(tm.tm_gmtoff / 60)
        .ok()
        .filter(|offset| (-24 * 60..=24 * 60).contains(offset))?;
    Some(LocalDateTime {
        year,
        month,
        day,
        weekday,
        hour,
        minute,
        second,
        utc_offset_minutes,
    })
}

/// 非 Unix 平台暂不接系统时区，由调用方安全回退。
#[cfg(not(unix))]
pub fn local_parts(_unix_secs: u64) -> Option<LocalDateTime> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
        let y = year - i64::from(month <= 2);
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let m = if month > 2 { month - 3 } else { month + 9 };
        let doy = (153 * m + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    #[cfg(unix)]
    #[test]
    fn local_parts_round_trips_back_to_unix_seconds() {
        for ts in [0_u64, 31_536_000, 1_767_225_600, 1_795_000_000] {
            let parts = local_parts(ts).expect("plausible timestamp should decompose on unix");
            assert!((1..=12).contains(&parts.month));
            assert!((1..=31).contains(&parts.day));
            assert!(parts.weekday <= 6);
            assert!(parts.hour <= 23);
            assert!(parts.minute <= 59);
            assert!(parts.second <= 60);
            let days = days_from_civil(
                i64::from(parts.year),
                i64::from(parts.month),
                i64::from(parts.day),
            );
            let local_secs = days * 86_400
                + i64::from(parts.hour) * 3_600
                + i64::from(parts.minute) * 60
                + i64::from(parts.second);
            let reconstructed = local_secs - i64::from(parts.utc_offset_minutes) * 60;
            assert_eq!(reconstructed, ts as i64, "ts={ts}, parts={parts:?}");
        }
    }

    #[test]
    fn out_of_range_u64_returns_none_without_overflow() {
        assert_eq!(local_parts(u64::MAX), None);
        assert_eq!(local_parts((i64::MAX as u64) + 1), None);
        assert_eq!(local_parts(i64::MAX as u64), None);
    }
}
