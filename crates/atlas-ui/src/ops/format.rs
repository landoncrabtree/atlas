//! Size and title formatting helpers for the ops panel.

use std::time::Duration;

use atlas_ops::OpKindDescriptor;

/// Format a byte count as a human-readable string.
///
/// # Examples
/// ```
/// use atlas_ui::ops::format::format_size;
/// assert_eq!(format_size(0), "0 B");
/// assert_eq!(format_size(1023), "1023 B");
/// assert_eq!(format_size(1024), "1.0 KiB");
/// ```
#[must_use]
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    const TIB: u64 = 1024 * GIB;

    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else if bytes < GIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes < TIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{:.2} TiB", bytes as f64 / TIB as f64)
    }
}

/// Format an [`OpKindDescriptor`] into the short panel title shown per-row.
///
/// Returns a string of the form `"<Kind>: <summary>"`.
#[must_use]
pub fn format_op_title(kind: &OpKindDescriptor) -> String {
    format!("{}: {}", kind.kind, kind.summary)
}

/// Return the Unicode glyph that represents an op kind in the progress modal.
///
/// Uses the same set of pictographs the app already uses in the shortcut
/// footer — no new assets, no colour, no branded icons.
#[must_use]
pub fn glyph_for_op_kind(kind: &str) -> &'static str {
    match kind {
        "Copy" => "📋",
        "Move" => "✂️",
        "Trash" => "🗑",
        "Delete" => "⨯",
        "Rename" => "✎",
        "Mkdir" => "📁",
        _ => "•",
    }
}

/// Build the header subtitle for the progress modal.
///
/// This mirrors [`format_op_title`] but drops the `"<Kind>: "` prefix — the
/// modal already carries a dedicated `title-text` that reads e.g.
/// `"Copying"`, `"Moving to Trash"`, so the subtitle only needs the summary
/// (`"3 items → /Users/foo/Documents"`).
#[must_use]
pub fn format_op_subtitle(kind: &OpKindDescriptor) -> String {
    kind.summary.clone()
}

/// Build the gerund heading for the progress modal (`"Copying"`, `"Moving"`).
///
/// Falls back to the raw kind label for unknown kinds.
#[must_use]
pub fn format_op_heading(kind: &str) -> &'static str {
    match kind {
        "Copy" => "Copying",
        "Move" => "Moving",
        "Trash" => "Moving to Trash",
        "Delete" => "Deleting",
        "Rename" => "Renaming",
        "Mkdir" => "Creating folder",
        _ => "Working",
    }
}

/// Format a coarse ETA string given `elapsed` and completion `fraction`.
///
/// Returns an empty string when a meaningful estimate isn't yet available
/// (fraction near zero, elapsed too short). Rounds to whole seconds under a
/// minute, `Xm Ys` under an hour, `Xh Ym` otherwise.
#[must_use]
pub fn format_eta(elapsed: Duration, fraction: f32) -> String {
    if !fraction.is_finite() || fraction <= 0.02 || elapsed < Duration::from_millis(500) {
        return String::new();
    }
    let f = fraction.clamp(0.0, 1.0) as f64;
    let total_secs = elapsed.as_secs_f64() / f;
    let remaining_secs = (total_secs - elapsed.as_secs_f64()).max(0.0);
    format_duration(Duration::from_secs_f64(remaining_secs))
}

/// Human-friendly duration formatter used by [`format_eta`].
#[must_use]
pub fn format_duration(dur: Duration) -> String {
    let secs = dur.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m {s}s")
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{h}h {m}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_zero() {
        assert_eq!(format_size(0), "0 B");
    }

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn format_size_kib() {
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(2048), "2.0 KiB");
    }

    #[test]
    fn format_size_mib() {
        assert_eq!(format_size(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn format_size_gib() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GiB");
    }

    #[test]
    fn format_op_title_copy() {
        let desc = OpKindDescriptor {
            kind: "Copy",
            summary: "3 items → /Downloads".to_owned(),
        };
        assert_eq!(format_op_title(&desc), "Copy: 3 items → /Downloads");
    }

    #[test]
    fn heading_and_glyph_for_known_kinds() {
        assert_eq!(format_op_heading("Copy"), "Copying");
        assert_eq!(format_op_heading("Trash"), "Moving to Trash");
        assert_eq!(format_op_heading("Mkdir"), "Creating folder");
        assert_eq!(format_op_heading("Whatever"), "Working");
        assert_eq!(glyph_for_op_kind("Copy"), "📋");
        assert_eq!(glyph_for_op_kind("Trash"), "🗑");
        assert_eq!(glyph_for_op_kind("Mkdir"), "📁");
        assert_eq!(glyph_for_op_kind("Zzz"), "•");
    }

    #[test]
    fn eta_empty_when_fraction_tiny() {
        assert_eq!(format_eta(Duration::from_secs(1), 0.0), "");
        assert_eq!(format_eta(Duration::from_secs(1), 0.001), "");
    }

    #[test]
    fn eta_reports_seconds_when_reasonable() {
        // 25% done after 5s → total ~20s, remaining ~15s (float rounding may
        // land on 14s vs 15s; both are acceptable coarse ETAs).
        let eta = format_eta(Duration::from_secs(5), 0.25);
        assert!(eta == "14s" || eta == "15s", "got {eta}");
    }

    #[test]
    fn eta_reports_minutes_and_hours_for_long_ops() {
        // 10% done after 30s → total 300s, remaining 270s = 4m 30s (± 1s).
        let eta_min = format_eta(Duration::from_secs(30), 0.10);
        assert!(eta_min == "4m 29s" || eta_min == "4m 30s", "got {eta_min}");
        // 10% done after 600s → total 6000s, remaining 5400s = 1h 30m.
        let eta_hour = format_eta(Duration::from_secs(600), 0.10);
        assert!(
            eta_hour == "1h 29m" || eta_hour == "1h 30m",
            "got {eta_hour}"
        );
    }
}
