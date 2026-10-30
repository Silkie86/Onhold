//! Time: cron schedules in a workspace's timezone, and how long a failed job
//! waits before its next attempt.

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;

/// Scheduled rules run at most this often.
pub const MIN_INTERVAL: chrono::Duration = chrono::Duration::minutes(5);

/// Parses a standard five-field cron expression (`minute hour day month weekday`).
pub fn parse_cron(expr: &str) -> Result<Cron, String> {
    if expr.split_whitespace().count() != 5 {
        return Err("use five fields: minute hour day-of-month month day-of-week".into());
    }
    let cron = Cron::from_str(expr).map_err(|e| format!("not a valid schedule: {e}"))?;
    // Two consecutive runs from a fixed point show the shortest gap well
    // enough for every pattern a person would write.
    let start = DateTime::<Utc>::from_timestamp(1_767_225_600, 0).unwrap_or_default();
    let first = cron
        .find_next_occurrence(&start, false)
        .map_err(|e| e.to_string())?;
    let second = cron
        .find_next_occurrence(&first, false)
        .map_err(|e| e.to_string())?;
    if second - first < MIN_INTERVAL {
        return Err("run at most every 5 minutes".into());
    }
    Ok(cron)
}

/// The next time `cron` fires strictly after `after`, read in `tz`.
pub fn next_run(cron: &Cron, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    cron.find_next_occurrence(&after.with_timezone(&tz), false)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Retry backoff: 30s, 1m, 2m, 4m … capped at 6 hours, each ±20% so a burst
/// of failures does not retry in lockstep.
pub fn retry_delay(attempt: i32) -> Duration {
    const BASE: u64 = 30;
    const CAP: u64 = 6 * 3600;
    let exp = attempt.saturating_sub(1).clamp(0, 20).cast_unsigned();
    let secs = BASE.saturating_mul(1u64 << exp).min(CAP);
    let mut byte = [0u8; 2];
    let jitter = if getrandom::fill(&mut byte).is_ok() {
        // -0.2 ..= +0.2
        (f64::from(u16::from_le_bytes(byte)) / f64::from(u16::MAX) - 0.5) * 0.4
    } else {
        0.0
    };
    Duration::from_secs_f64(secs as f64 * (1.0 + jitter))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn cron_runs_in_the_workspace_timezone() {
        let cron = parse_cron("0 9 * * MON").expect("valid");
        let london: Tz = "Europe/London".parse().expect("tz");
        // Sunday 2027-07-04 12:00 UTC → Monday 09:00 BST = 08:00 UTC.
        let after = Utc
            .with_ymd_and_hms(2027, 7, 4, 12, 0, 0)
            .single()
            .expect("time");
        let next = next_run(&cron, london, after).expect("next");
        assert_eq!(
            next,
            Utc.with_ymd_and_hms(2027, 7, 5, 8, 0, 0)
                .single()
                .expect("time")
        );
        // In winter, GMT = UTC.
        let after = Utc
            .with_ymd_and_hms(2027, 1, 3, 12, 0, 0)
            .single()
            .expect("time");
        let next = next_run(&cron, london, after).expect("next");
        assert_eq!(
            next,
            Utc.with_ymd_and_hms(2027, 1, 4, 9, 0, 0)
                .single()
                .expect("time")
        );
    }

    #[test]
    fn bad_or_too_frequent_schedules_are_refused() {
        assert!(parse_cron("*/5 * * * *").is_ok());
        for bad in [
            "* * * * *",
            "*/2 * * * *",
            "0 9 * *",
            "0 0 9 * * *",
            "banana",
        ] {
            assert!(parse_cron(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn backoff_grows_and_caps() {
        let secs = |a| retry_delay(a).as_secs_f64();
        assert!((24.0..=36.0).contains(&secs(1)));
        assert!((48.0..=72.0).contains(&secs(2)));
        assert!((96.0..=144.0).contains(&secs(3)));
        assert!(secs(40) <= 6.0 * 3600.0 * 1.2);
        assert!(secs(0) > 0.0);
    }
}
