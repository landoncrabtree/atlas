//! Size and title formatting helpers for the ops panel.

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
}
