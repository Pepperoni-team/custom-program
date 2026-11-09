//! UTC calendar arithmetic for the monthly cycle (spec §4.6, D11). A month is
//! identified by its index `year * 12 + (month - 1)`: October 2026 is 24,321.

use anchor_lang::prelude::*;

use crate::raise::constants::{DECLARE_DAY_OF_NEXT_MONTH, DUE_DAY_OF_SECOND_MONTH};
use crate::errors::FanexError;

pub const SECONDS_PER_DAY: i64 = 86_400;

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant, days_from_civil).
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day count since 1970-01-01 (H. Hinnant, civil_from_days).
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The month index of a UTC timestamp.
pub fn month_index(unix_ts: i64) -> Result<u32> {
    let (year, month, _) = civil_from_days(unix_ts.div_euclid(SECONDS_PER_DAY));
    u32::try_from(year * 12 + (month - 1)).map_err(|_| error!(FanexError::RaiseMathOverflow))
}

/// 00:00 UTC of the first day of month `index`.
pub fn month_start_ts(index: u32) -> i64 {
    let year = (index / 12) as i64;
    let month = (index % 12) as i64 + 1;
    days_from_civil(year, month, 1) * SECONDS_PER_DAY
}

/// The first revenue month of a raise that succeeds: the first full month after it closed, at
/// its deadline or, sold out, at the finalize that saw it so. A finalize sent after the
/// deadline (the keeper runs every 30 minutes, and can be late) keeps the deadline's month:
/// the realism limit the attester signed priced the month after the deadline, and a term moved
/// a month on pays fans one month less of a declining revenue (2026-10-12).
pub fn first_paying_month(now: i64, deadline: i64) -> Result<u32> {
    Ok(month_index(now.min(deadline))? + 1)
}

/// First moment month `index` may be declared: the 15th of the next month, 00:00 UTC.
pub fn declare_open_ts(index: u32) -> i64 {
    month_start_ts(index + 1) + (DECLARE_DAY_OF_NEXT_MONTH - 1) * SECONDS_PER_DAY
}

/// First moment month `index` is overdue: the 6th of the month after next,
/// 00:00 UTC, so the creator may pay through the whole 5th.
pub fn due_ts(index: u32) -> i64 {
    month_start_ts(index + 2) + (DUE_DAY_OF_SECOND_MONTH - 1) * SECONDS_PER_DAY
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-01 00:00:00 UTC.
    const OCT_1_2026: i64 = 1_790_812_800;
    const OCTOBER_2026: u32 = 24_321;

    #[test]
    fn the_epoch_is_day_zero() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn a_known_date_lands_on_its_timestamp() {
        assert_eq!(days_from_civil(2026, 10, 1) * SECONDS_PER_DAY, OCT_1_2026);
    }

    #[test]
    fn a_leap_day_round_trips() {
        let leap = days_from_civil(2028, 2, 29);
        assert_eq!(civil_from_days(leap), (2028, 2, 29));
        assert_eq!(civil_from_days(leap + 1), (2028, 3, 1));
    }

    #[test]
    fn every_day_from_1970_to_2100_round_trips() {
        let end = days_from_civil(2100, 1, 1);
        for day in 0..end {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
    }

    #[test]
    fn october_2026_has_its_index_and_its_start() {
        assert_eq!(month_index(OCT_1_2026).unwrap(), OCTOBER_2026);
        assert_eq!(month_index(OCT_1_2026 - 1).unwrap(), OCTOBER_2026 - 1);
        assert_eq!(month_start_ts(OCTOBER_2026), OCT_1_2026);
    }

    #[test]
    fn october_is_declared_from_november_15_and_overdue_from_december_6() {
        assert_eq!(declare_open_ts(OCTOBER_2026), 1_794_700_800);
        assert_eq!(due_ts(OCTOBER_2026), 1_796_515_200);
    }

    #[test]
    fn a_finalize_after_the_deadline_keeps_the_deadlines_month() {
        // Deadline 2026-10-31 23:40 UTC, finalized by the keeper's run at 2026-11-01 00:00.
        let deadline = days_from_civil(2026, 10, 31) * SECONDS_PER_DAY + 23 * 3_600 + 40 * 60;
        let next_run = days_from_civil(2026, 11, 1) * SECONDS_PER_DAY;
        assert_eq!(first_paying_month(next_run, deadline).unwrap(), OCTOBER_2026 + 1);
        assert_eq!(first_paying_month(next_run + 40 * SECONDS_PER_DAY, deadline).unwrap(), OCTOBER_2026 + 1);
    }

    #[test]
    fn a_sell_out_before_the_deadline_starts_from_the_month_after_the_sale() {
        let sold_out = days_from_civil(2026, 10, 20) * SECONDS_PER_DAY;
        let deadline = days_from_civil(2026, 11, 15) * SECONDS_PER_DAY;
        assert_eq!(first_paying_month(sold_out, deadline).unwrap(), OCTOBER_2026 + 1);
    }

    #[test]
    fn december_rolls_into_the_next_year() {
        let december_2026 = 24_323;
        assert_eq!(declare_open_ts(december_2026), days_from_civil(2027, 1, 15) * SECONDS_PER_DAY);
        assert_eq!(due_ts(december_2026), days_from_civil(2027, 2, 6) * SECONDS_PER_DAY);
    }
}
