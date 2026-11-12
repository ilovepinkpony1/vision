//! Time in UTC: RFC 3339 text for the ledger and the files, and the compact form of turn ids.
//! The local time of the machine only serves a time of day of the adapter, such as the `from`
//! of a wait step, and the texts for people.

use anyhow::{Context, Result};
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{Duration, OffsetDateTime, PrimitiveDateTime, UtcOffset};

/// The time now in UTC, with whole seconds.
pub fn now() -> OffsetDateTime {
    whole_seconds(OffsetDateTime::now_utc())
}

/// The time now as RFC 3339 UTC text, such as `2026-11-16T14:30:00Z`.
pub fn now_rfc3339() -> String {
    rfc3339(now())
}

/// RFC 3339 UTC text with whole seconds, such as `2026-11-16T14:30:00Z`.
pub fn rfc3339(at: OffsetDateTime) -> String {
    let at = at.to_offset(UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// Reads RFC 3339 text with any offset, and gives the time in UTC.
pub fn parse_rfc3339(text: &str) -> Result<OffsetDateTime> {
    let at = OffsetDateTime::parse(text.trim(), &Rfc3339).with_context(|| {
        format!("`{text}` is not an RFC 3339 time, such as 2026-11-16T14:30:00Z")
    })?;
    Ok(at.to_offset(UtcOffset::UTC))
}

/// The compact UTC form of a turn id, such as `20261116T143000Z`.
pub fn compact(at: OffsetDateTime) -> String {
    let at = at.to_offset(UtcOffset::UTC);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// Reads the compact UTC form, such as `20261116T143000Z`.
pub fn parse_compact(text: &str) -> Result<OffsetDateTime> {
    let format = format_description!("[year][month][day]T[hour][minute][second]Z");
    let at = PrimitiveDateTime::parse(text, format)
        .with_context(|| format!("`{text}` is not a compact UTC time, such as 20261116T143000Z"))?;
    Ok(at.assume_utc())
}

/// Whole seconds from `from` to `to`. Negative when `to` is earlier.
pub fn seconds_between(from: OffsetDateTime, to: OffsetDateTime) -> i64 {
    (to - from).whole_seconds()
}

/// A short age for people, such as `12s`, `5m`, `3h`, or `2d`.
pub fn short_age(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

fn whole_seconds(at: OffsetDateTime) -> OffsetDateTime {
    at - Duration::nanoseconds(i64::from(at.nanosecond()))
}

/// The offset of the local time of the machine from UTC in seconds, at the moment `at` (seconds
/// since 1970). It follows the time zone of the machine and its changes of the clock. 0 when the
/// system cannot say.
pub fn local_offset(at: i64) -> i32 {
    let t: libc::time_t = at as libc::time_t;
    // SAFETY: localtime_r writes only into the local value that it gets.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i32
    }
}

/// The first moment after `after` whose local time is `hour:minute`, in UTC. `offset` gives the
/// offset of the local time at a moment, such as [`local_offset`]. A moment at that time on the
/// day of `after` counts only when it comes later than `after`; else the moment is on the next
/// day. Near a change of the clock, the offset of the new moment wins.
pub fn next_local_at(
    after: OffsetDateTime,
    hour: u8,
    minute: u8,
    offset: impl Fn(i64) -> i32,
) -> OffsetDateTime {
    let after = after.to_offset(UtcOffset::UTC);
    let shift = Duration::seconds(i64::from(offset(after.unix_timestamp())));
    // The wall clock of the machine, written as a UTC time.
    let wall = after + shift;
    let time =
        time::Time::from_hms(hour.min(23), minute.min(59), 0).unwrap_or(time::Time::MIDNIGHT);
    let mut wanted = PrimitiveDateTime::new(wall.date(), time).assume_utc();
    if wanted <= wall {
        wanted += Duration::days(1);
    }
    let guess = wanted - shift;
    let moment = wanted - Duration::seconds(i64::from(offset(guess.unix_timestamp())));
    if moment > after {
        moment
    } else {
        guess
    }
}

/// [`next_local_at`] in the time zone of the machine.
pub fn next_local(after: OffsetDateTime, hour: u8, minute: u8) -> OffsetDateTime {
    next_local_at(after, hour, minute, local_offset)
}

/// The local time of a moment as `HH:MM`, such as `09:00`.
pub fn local_hm(at: OffsetDateTime) -> String {
    let local = at + Duration::seconds(i64::from(local_offset(at.unix_timestamp())));
    format!("{:02}:{:02}", local.hour(), local.minute())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn rfc3339_is_utc_with_whole_seconds() {
        let at = datetime!(2026-11-16 16:30:00.75 +02:00);
        assert_eq!(rfc3339(at), "2026-11-16T14:30:00Z");
        assert_eq!(
            parse_rfc3339("2026-11-16T16:30:00+02:00").unwrap(),
            datetime!(2026-11-16 14:30:00 UTC)
        );
        assert!(parse_rfc3339("yesterday").is_err());
    }

    #[test]
    fn compact_round_trips() {
        let at = datetime!(2026-11-16 14:30:00 UTC);
        assert_eq!(compact(at), "20261116T143000Z");
        assert_eq!(parse_compact("20261116T143000Z").unwrap(), at);
        assert!(parse_compact("2026-11-16T14:30:00Z").is_err());
    }

    #[test]
    fn short_age_picks_one_unit() {
        assert_eq!(short_age(-5), "0s");
        assert_eq!(short_age(12), "12s");
        assert_eq!(short_age(300), "5m");
        assert_eq!(short_age(3 * 3600 + 5), "3h");
        assert_eq!(short_age(2 * 86400), "2d");
    }

    #[test]
    fn now_has_whole_seconds() {
        assert_eq!(now().nanosecond(), 0);
    }

    #[test]
    fn the_next_local_time_comes_after_the_entry() {
        // A fixed clock in a zone two hours east of UTC: 09:00 local is 07:00 UTC.
        let east = |_: i64| 2 * 3600;
        let next = |at| next_local_at(at, 9, 0, east);
        // An entry at 23:00 local starts at 09:00 the next day.
        assert_eq!(
            next(datetime!(2026-11-16 21:00:00 UTC)),
            datetime!(2026-11-17 07:00:00 UTC)
        );
        // An entry at 08:00 local starts at 09:00 the same day.
        assert_eq!(
            next(datetime!(2026-11-16 06:00:00 UTC)),
            datetime!(2026-11-16 07:00:00 UTC)
        );
        // An entry at 09:30 local starts at 09:00 the next day.
        assert_eq!(
            next(datetime!(2026-11-16 07:30:00 UTC)),
            datetime!(2026-11-17 07:00:00 UTC)
        );
        // An entry at 09:00 exactly: the moment must come after the entry.
        assert_eq!(
            next(datetime!(2026-11-16 07:00:00 UTC)),
            datetime!(2026-11-17 07:00:00 UTC)
        );
        // The local date can differ from the UTC date: 00:30 local on the 17th is 22:30 UTC on
        // the 16th, and 09:00 of the 17th comes next.
        assert_eq!(
            next(datetime!(2026-11-16 22:30:00 UTC)),
            datetime!(2026-11-17 07:00:00 UTC)
        );
        // In UTC the local time is the UTC time.
        assert_eq!(
            next_local_at(datetime!(2026-11-16 23:00:00 UTC), 9, 0, |_| 0),
            datetime!(2026-11-17 09:00:00 UTC)
        );
    }

    #[test]
    fn a_change_of_the_clock_overnight_keeps_the_local_time() {
        // The clock goes from +01:00 to +02:00 at 2026-03-29 01:00 UTC.
        let change = datetime!(2026-03-29 01:00:00 UTC).unix_timestamp();
        let zone = move |at: i64| if at < change { 3600 } else { 7200 };
        // An entry at 23:00 local (+01:00) starts at 09:00 local (+02:00), which is 07:00 UTC.
        assert_eq!(
            next_local_at(datetime!(2026-03-28 22:00:00 UTC), 9, 0, zone),
            datetime!(2026-03-29 07:00:00 UTC)
        );
    }
}
