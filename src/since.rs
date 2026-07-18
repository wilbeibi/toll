use anyhow::{bail, Context, Result};
use jiff::{Span, Timestamp, Zoned};

/// Convert a `--since` spec into an RFC-3339 UTC lower bound. Stored `ts`
/// values are always UTC RFC-3339 (`Timestamp::now().to_string()`), so the
/// bound is applied lexicographically via `WHERE ts >= ?`.
///
/// Accepted forms: `30m` / `12h` / `7d` (relative to now), `today` (local
/// midnight), a full RFC-3339 instant, or a date prefix like `2026-07-01`
/// (compares as a prefix, which includes the whole day onward).
pub fn lower_bound(spec: &str) -> Result<String> {
    let s = spec.trim();
    if s.eq_ignore_ascii_case("today") {
        let now = Zoned::now();
        let midnight = now
            .datetime()
            .date()
            .to_zoned(now.time_zone().clone())
            .context("resolving local midnight")?;
        return Ok(midnight.timestamp().to_string());
    }
    if let Some(span) = parse_relative(s)? {
        let start = Zoned::now()
            .checked_sub(span)
            .with_context(|| format!("--since {spec:?} is out of range"))?;
        return Ok(start.timestamp().to_string());
    }
    if s.contains('T') || s.contains(':') {
        let ts: Timestamp = s
            .parse()
            .with_context(|| format!("--since {spec:?} is not a valid RFC-3339 instant"))?;
        return Ok(ts.to_string());
    }
    if s.len() >= 4 && s.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Ok(s.to_string());
    }
    bail!("bad --since {spec:?}; expected 30m, 12h, 7d, today, a date (2026-07-01), or an RFC-3339 instant");
}

/// `Nm`/`Nh`/`Nd` → a Span; None when the spec is not in that shape.
fn parse_relative(s: &str) -> Result<Option<Span>> {
    let Some(unit) = s.chars().last() else {
        return Ok(None);
    };
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let n: i64 = digits
        .parse()
        .with_context(|| format!("bad --since amount {digits:?}"))?;
    let span = match unit {
        'm' => Span::new().try_minutes(n),
        'h' => Span::new().try_hours(n),
        'd' => Span::new().try_days(n),
        _ => return Ok(None),
    };
    Ok(Some(span.with_context(|| {
        format!("--since amount {n} is out of range")
    })?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_hours_is_in_the_past_and_utc() {
        let bound = lower_bound("2h").unwrap();
        let now = Timestamp::now().to_string();
        assert!(bound < now, "{bound} should sort before {now}");
        assert!(bound.ends_with('Z'));
    }

    #[test]
    fn relative_days_parse() {
        // Days are calendar units; must go through Zoned, not Timestamp.
        assert!(lower_bound("7d").is_ok());
        assert!(lower_bound("30m").is_ok());
    }

    #[test]
    fn date_prefix_passes_through() {
        assert_eq!(lower_bound("2026-07-01").unwrap(), "2026-07-01");
    }

    #[test]
    fn rfc3339_is_normalized_to_utc() {
        // Offset forms don't sort against stored `...Z` strings; they must
        // be normalized.
        let bound = lower_bound("2026-07-18T10:00:00+08:00").unwrap();
        assert_eq!(bound, "2026-07-18T02:00:00Z");
    }

    #[test]
    fn today_is_a_valid_bound() {
        let bound = lower_bound("today").unwrap();
        assert!(bound < Timestamp::now().to_string());
    }

    #[test]
    fn garbage_is_rejected() {
        for bad in ["yesterday", "5x", "m", "", "-3d"] {
            assert!(lower_bound(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
