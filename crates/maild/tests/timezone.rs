//! 系统时区（`TZ` + `localtime_r`）独立进程集成测试。
//!
//! 放在独立测试二进制中串行切换 `TZ`，避免与其他并行测试线程共享进程环境。

#[cfg(unix)]
#[test]
fn follow_system_timezone_and_reply_attribution_respect_tz_and_dst() {
    use maild::compose;
    use maild::localtime;
    use maild::rules::{is_quiet_at, QuietHoursConfig, RulesConfig};

    struct TzGuard(Option<std::ffi::OsString>);
    impl Drop for TzGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(prev) => std::env::set_var("TZ", prev),
                None => std::env::remove_var("TZ"),
            }
            localtime::refresh_timezone();
        }
    }
    let _guard = TzGuard(std::env::var_os("TZ"));

    // 1. Asia/Shanghai (UTC+08:00): 2026-11-18 11:35:00Z == 2026-11-18 19:35:00+08:00 (周三)
    std::env::set_var("TZ", "Asia/Shanghai");
    let shanghai_ts = 1_795_001_700_u64;
    let parts = localtime::local_parts(shanghai_ts).expect("Asia/Shanghai local_parts");
    assert_eq!(parts.year, 2026);
    assert_eq!(parts.month, 11);
    assert_eq!(parts.day, 18);
    assert_eq!(parts.weekday, 3);
    assert_eq!(parts.hour, 19);
    assert_eq!(parts.minute, 35);
    assert_eq!(parts.utc_offset_minutes, 480);
    assert_eq!(
        compose::attribution_date(&maild_l10n::ZH_HANS.quote, shanghai_ts),
        "2026年11月18日 周三 19:35（UTC+08:00）"
    );

    // 2. America/Chicago 夏令时切换：同一本地静默窗口 22:00-07:00 在冬令时（UTC-6）与夏令时（UTC-5）自动平移。
    std::env::set_var("TZ", "America/Chicago");
    let cfg = RulesConfig {
        quiet_hours: vec![QuietHoursConfig {
            start: "22:00".into(),
            end: "07:00".into(),
        }],
        timezone_offset_minutes: 0, // follow_system_timezone = true 时忽略固定偏移
        follow_system_timezone: true,
        ..RulesConfig::default()
    };
    // 冬令时 CST (UTC-6)：2026-01-15 03:30 UTC = 当地前一日 21:30（未入静默），04:30 UTC = 22:30（已入静默）
    assert!(!is_quiet_at(&cfg, 1_768_447_800));
    assert!(is_quiet_at(&cfg, 1_768_451_400));
    // 夏令时 CDT (UTC-5)：2026-07-15 03:30 UTC = 当地前一日 22:30（已入静默！）
    assert!(is_quiet_at(&cfg, 1_784_086_200));

    // 3. 半小时时区 Asia/Kolkata (UTC+05:30)
    std::env::set_var("TZ", "Asia/Kolkata");
    assert!(
        compose::attribution_date(&maild_l10n::ZH_HANS.quote, shanghai_ts).ends_with("（UTC+05:30）"),
        "actual={}",
        compose::attribution_date(&maild_l10n::ZH_HANS.quote, shanghai_ts)
    );
}
