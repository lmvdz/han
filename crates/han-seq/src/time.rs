//! HAN Times (`YYYY-MM-DDTHH:MM:SSZ`) from and to seconds since the epoch.

pub use han_core::grammar::epoch_seconds as parse_time;

/// The HAN Time of `secs` seconds since 1970-01-01T00:00:00Z (years 0001 to 9999).
pub fn format_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for s in [0, 1, 86_399, 951_782_400, 1_767_225_600, 4_102_444_800] {
            assert_eq!(parse_time(&format_time(s)), Some(s));
        }
        assert_eq!(format_time(1_767_225_600), "2026-01-01T00:00:00Z");
    }
}
