//! Parsing of durations given on the command line, such as `--timeout 20m`.

/// Parse `90s`, `20m`, `2h` or a plain integer (seconds) into whole seconds.
///
/// Only the three suffixes are accepted, and the number must be a whole
/// non-negative integer. Range checks belong to request validation, not here.
pub fn parse_duration_secs(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (digits, multiplier) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1),
        Some('m') => (&text[..text.len() - 1], 60),
        Some('h') => (&text[..text.len() - 1], 3600),
        _ => (text, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "invalid duration {text:?}: use a whole number of seconds, or a number with s, m or h, such as 90s, 20m, 2h"
        ));
    }
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .ok_or_else(|| format!("duration {text:?} is too large"))
}

#[cfg(test)]
mod tests {
    use super::parse_duration_secs as parse;

    #[test]
    fn suffixes_convert_to_seconds() {
        assert_eq!(parse("90s"), Ok(90));
        assert_eq!(parse("20m"), Ok(1200));
        assert_eq!(parse("2h"), Ok(7200));
    }

    #[test]
    fn plain_integer_means_seconds() {
        assert_eq!(parse("45"), Ok(45));
        assert_eq!(parse(" 45 "), Ok(45));
        assert_eq!(parse("0"), Ok(0));
    }

    #[test]
    fn malformed_durations_are_errors() {
        for bad in [
            "", "s", "m", "1d", "1.5m", "-5", "5 m", "m5", "1h30m", "+3", "abc",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn overflow_is_an_error_not_a_wraparound() {
        assert!(parse("99999999999999999999").is_err());
        assert!(parse(&format!("{}h", u64::MAX)).is_err());
    }
}
