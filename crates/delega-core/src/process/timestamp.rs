//! RFC 3339 timestamps in UTC, without a time-zone database.

use std::time::{SystemTime, UNIX_EPOCH};

/// Format a time as `YYYY-MM-DDTHH:MM:SS.mmmZ`. A time before the Unix epoch
/// is formatted as the epoch.
pub fn rfc3339_utc(time: SystemTime) -> String {
    let ms = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    format_millis(ms)
}

fn format_millis(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_millis(1_000_000_000_123), "2001-09-09T01:46:40.123Z");
        // A leap day.
        assert_eq!(format_millis(1_709_164_800_000), "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn a_time_before_the_epoch_is_the_epoch() {
        let before = UNIX_EPOCH - Duration::from_secs(5);
        assert_eq!(rfc3339_utc(before), "1970-01-01T00:00:00.000Z");
    }
}
