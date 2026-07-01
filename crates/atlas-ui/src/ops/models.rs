//! Row model for the ops panel.

use slint::SharedString;

use super::format::format_size;

/// One file-operation row as tracked by [`super::controller::OpsController`].
///
/// This is the Rust-side representation; call [`OpRow::to_slint`] to convert
/// it to the Slint-generated [`crate::OpRow`] struct before pushing to the UI.
#[derive(Debug, Clone, Default)]
pub struct OpRow {
    /// Stable `OpId` (the raw `u64` from `atlas_ops`).
    pub id: u64,
    /// Short title shown in the panel (e.g. `"Copy: 3 items → Downloads"`).
    pub title: String,
    /// Human-readable status string.
    pub status: String,
    /// Completion fraction in `[0.0, 1.0]`.
    pub progress: f32,
    /// Total logical items involved in the operation.
    pub items_total: u64,
    /// Completed logical items.
    pub items_done: u64,
    /// Total bytes (raw) — stored separately for progress computation.
    pub bytes_total_raw: u64,
    /// Done bytes (raw).
    pub bytes_done_raw: u64,
    /// Currently processing path (may be empty).
    pub current_path: String,
    /// `true` when the op has reached a terminal state (done / failed / cancelled).
    pub is_terminal: bool,
    /// `true` when the op finished with an error.
    pub is_error: bool,
}

impl OpRow {
    /// Convert to the Slint-generated [`crate::OpRow`] struct for UI rendering.
    #[must_use]
    pub fn to_slint(&self) -> crate::OpRow {
        crate::OpRow {
            id: SharedString::from(self.id.to_string().as_str()),
            title: SharedString::from(self.title.as_str()),
            status: SharedString::from(self.status.as_str()),
            progress: self.progress,
            items_total: self.items_total.min(i32::MAX as u64) as i32,
            items_done: self.items_done.min(i32::MAX as u64) as i32,
            bytes_total: SharedString::from(format_size(self.bytes_total_raw).as_str()),
            bytes_done: SharedString::from(format_size(self.bytes_done_raw).as_str()),
            current_path: SharedString::from(self.current_path.as_str()),
            is_terminal: self.is_terminal,
            is_error: self.is_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_row_progress_fraction() {
        let row = OpRow {
            id: 1,
            title: "Copy: test.txt → /tmp".to_owned(),
            status: "Running".to_owned(),
            progress: 0.5,
            items_total: 10,
            items_done: 5,
            bytes_total_raw: 2048,
            bytes_done_raw: 1024,
            current_path: String::new(),
            is_terminal: false,
            is_error: false,
        };
        assert!((row.progress - 0.5).abs() < f32::EPSILON);
        assert_eq!(row.items_done, 5);
        assert_eq!(row.items_total, 10);
    }

    #[test]
    fn op_row_to_slint_formats_bytes() {
        let row = OpRow {
            bytes_done_raw: 1024,
            bytes_total_raw: 2048,
            ..OpRow::default()
        };
        let slint_row = row.to_slint();
        assert_eq!(slint_row.bytes_done.as_str(), "1.0 KiB");
        assert_eq!(slint_row.bytes_total.as_str(), "2.0 KiB");
    }

    #[test]
    fn op_row_terminal_flags() {
        let row = OpRow {
            is_terminal: true,
            is_error: true,
            ..Default::default()
        };
        let slint_row = row.to_slint();
        assert!(slint_row.is_terminal);
        assert!(slint_row.is_error);
    }
}
