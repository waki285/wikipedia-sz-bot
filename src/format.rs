//! Formatting helpers shared by maintenance reports and progress logs.

use std::time::Duration;

/// Format a number with thousands separators, e.g. `1013254` -> `1,013,254`.
#[must_use]
pub fn count(count: u64) -> String {
    let digits = count.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

/// Format a duration compactly for a progress log, e.g. `3h30m`, `12m05s`.
///
/// Only the two largest non-zero units are shown, which is precise enough to
/// follow a scan that runs for hours.
#[must_use]
pub fn duration(value: Duration) -> String {
    let seconds = value.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_counts() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1_013_254), "1,013,254");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(Duration::from_secs(0)), "0s");
        assert_eq!(duration(Duration::from_secs(45)), "45s");
        assert_eq!(duration(Duration::from_secs(125)), "2m05s");
        assert_eq!(duration(Duration::from_hours(1)), "1h00m");
        assert_eq!(duration(Duration::from_mins(210)), "3h30m");
    }
}
