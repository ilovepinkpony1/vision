//! Time in UTC: RFC 3339 text for the ledger and the files, and the compact form of turn ids.

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
}
