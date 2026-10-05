// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `meta.received_at`: a receipt time, Unix-epoch milliseconds, written as
//! RFC3339 UTC with milliseconds (`2026-10-05T19:07:02.497Z`).
//!
//! Written here rather than taken from a date crate: the conversion is the
//! civil-from-days arithmetic below, and a dependency for one function is
//! a graph the bridge would carry for nothing.

/// The last instant RFC3339's four-digit year can write:
/// 9999-12-31T23:59:59.999Z.
pub const MAX_RECEIVED_AT_MS: u64 = 253_402_300_799_999;

/// `ms` as RFC3339 UTC, or `None` past [`MAX_RECEIVED_AT_MS`], which a
/// four-digit year cannot write. A receipt time is the daemon's wall
/// clock, so `None` is a clock the conversion refuses rather than a time
/// it shows wrongly.
#[must_use]
pub fn rfc3339_utc(ms: u64) -> Option<String> {
    if ms > MAX_RECEIVED_AT_MS {
        return None;
    }
    let millis = ms % 1000;
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
    ))
}

/// The proleptic Gregorian date `days` after 1970-01-01, by eras of
/// 146 097 days (400 years), each starting on a 1 March so the leap day
/// is the era-year's last.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468; // days since 0000-03-01
    let era = z / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March first
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectors across the arithmetic's edges: the epoch, a leap day in a
    /// year divisible by 400 and in one divisible only by 4, the day after
    /// a century's skipped leap day, a year boundary at its last
    /// millisecond, and the last writable instant.
    #[test]
    fn known_instants_format_exactly() {
        for (ms, want) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (1_709_164_800_123, "2024-02-29T00:00:00.123Z"),
            (4_107_542_400_000, "2100-03-01T00:00:00.000Z"),
            (1_704_067_199_999, "2023-12-31T23:59:59.999Z"),
            (1_791_227_222_497, "2026-10-05T19:07:02.497Z"),
            (MAX_RECEIVED_AT_MS, "9999-12-31T23:59:59.999Z"),
        ] {
            assert_eq!(rfc3339_utc(ms).as_deref(), Some(want), "{ms}");
        }
    }

    #[test]
    fn past_the_four_digit_year_is_refused() {
        assert_eq!(rfc3339_utc(MAX_RECEIVED_AT_MS + 1), None);
        assert_eq!(rfc3339_utc(u64::MAX), None);
    }

    /// Every day for 800 years round-trips through the inverse, so the era
    /// arithmetic is checked everywhere, not only at the vectors.
    #[test]
    fn every_day_for_eight_centuries_is_the_day_after_the_one_before() {
        let mut previous = civil_from_days(0);
        assert_eq!(previous, (1970, 1, 1));
        for days in 1..(800 * 366) {
            let (y, m, d) = civil_from_days(days);
            let (py, pm, pd) = previous;
            let next_day = (y, m, d) == (py, pm, pd + 1);
            let next_month = (y, m, d) == (py, pm + 1, 1);
            let next_year = (y, m, d) == (py + 1, 1, 1) && (pm, pd) == (12, 31);
            assert!(
                next_day || next_month || next_year,
                "{previous:?} -> {:?}",
                (y, m, d)
            );
            if next_month || next_year {
                let leap = py % 4 == 0 && (py % 100 != 0 || py % 400 == 0);
                let last = match pm {
                    2 if leap => 29,
                    2 => 28,
                    4 | 6 | 9 | 11 => 30,
                    _ => 31,
                };
                assert_eq!(pd, last, "{py}-{pm} ended on {pd}");
            }
            previous = (y, m, d);
        }
    }
}
