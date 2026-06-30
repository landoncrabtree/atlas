//! Formatting helpers for file sizes and relative timestamps.

use std::time::SystemTime;

/// Format a byte count using binary (KiB/MiB/GiB/TiB) units.
///
/// Returns `"—"` for zero (typically directories), `"N B"` below 1 KiB,
/// and `"X.Y KiB"` / `"X.Y MiB"` etc. for larger values.
#[must_use]
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    const TIB: u64 = 1024 * GIB;

    if bytes == 0 {
        return "—".to_owned();
    }
    if bytes < KIB {
        return format!("{bytes} B");
    }
    if bytes < MIB {
        return format!("{:.1} KiB", bytes as f64 / KIB as f64);
    }
    if bytes < GIB {
        return format!("{:.1} MiB", bytes as f64 / MIB as f64);
    }
    if bytes < TIB {
        return format!("{:.1} GiB", bytes as f64 / GIB as f64);
    }
    format!("{:.1} TiB", bytes as f64 / TIB as f64)
}

/// Format a [`SystemTime`] as a human-readable relative string.
///
/// Produces strings like `"just now"`, `"5 minutes ago"`, `"2 hours ago"`,
/// `"yesterday"`, `"3 days ago"`, `"last month"`, or `"2 years ago"`.
/// Future timestamps are rendered as `"just now"`.
#[must_use]
pub fn format_relative_time(time: SystemTime) -> String {
    let now = SystemTime::now();
    let elapsed = match now.duration_since(time) {
        Ok(duration) => duration,
        Err(_) => return "just now".to_owned(),
    };

    let secs = elapsed.as_secs();
    if secs < 60 {
        return "just now".to_owned();
    }
    let mins = secs / 60;
    if mins < 60 {
        return if mins == 1 {
            "1 minute ago".to_owned()
        } else {
            format!("{mins} minutes ago")
        };
    }
    let hours = mins / 60;
    if hours < 24 {
        return if hours == 1 {
            "1 hour ago".to_owned()
        } else {
            format!("{hours} hours ago")
        };
    }
    let days = hours / 24;
    if days == 1 {
        return "yesterday".to_owned();
    }
    if days < 30 {
        return format!("{days} days ago");
    }
    let months = days / 30;
    if months == 1 {
        return "last month".to_owned();
    }
    if months < 12 {
        return format!("{months} months ago");
    }
    let years = months / 12;
    if years == 1 {
        return "1 year ago".to_owned();
    }
    format!("{years} years ago")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn format_size_zero() {
        assert_eq!(format_size(0), "—");
    }

    #[test]
    fn format_size_below_kib() {
        assert_eq!(format_size(1), "1 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn format_size_kib() {
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
    }

    #[test]
    fn format_size_mib() {
        assert_eq!(format_size(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn format_size_gib() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GiB");
    }

    #[test]
    fn format_relative_just_now() {
        let t = SystemTime::now() - Duration::from_secs(30);
        assert_eq!(format_relative_time(t), "just now");
    }

    #[test]
    fn format_relative_minutes() {
        let t = SystemTime::now() - Duration::from_secs(5 * 60 + 10);
        assert_eq!(format_relative_time(t), "5 minutes ago");
    }

    #[test]
    fn format_relative_one_hour() {
        let t = SystemTime::now() - Duration::from_secs(65 * 60);
        assert_eq!(format_relative_time(t), "1 hour ago");
    }

    #[test]
    fn format_relative_hours() {
        let t = SystemTime::now() - Duration::from_secs(3 * 3600 + 5);
        assert_eq!(format_relative_time(t), "3 hours ago");
    }

    #[test]
    fn format_relative_yesterday() {
        let t = SystemTime::now() - Duration::from_secs(25 * 3600);
        assert_eq!(format_relative_time(t), "yesterday");
    }

    #[test]
    fn format_relative_days() {
        let t = SystemTime::now() - Duration::from_secs(3 * 24 * 3600 + 60);
        assert_eq!(format_relative_time(t), "3 days ago");
    }

    #[test]
    fn format_relative_last_month() {
        let t = SystemTime::now() - Duration::from_secs(31 * 24 * 3600);
        assert_eq!(format_relative_time(t), "last month");
    }

    #[test]
    fn format_relative_years() {
        let t = SystemTime::now() - Duration::from_secs(2 * 365 * 24 * 3600 + 60);
        assert_eq!(format_relative_time(t), "2 years ago");
    }
}
