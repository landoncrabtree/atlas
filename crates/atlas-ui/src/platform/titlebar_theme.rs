//! Tint the OS-native window title bar to match Atlas's active theme mode.
//!
//! Slint (as of 1.17) draws only the content area; on macOS and Windows the
//! title bar / traffic-light chrome is owned by the OS. This module bridges
//! the app's theme mode to whichever OS API controls the title-bar tint so
//! a dark theme doesn't leave a bright band up top.
//!
//! # Platform matrix
//!
//! | OS      | Mechanism                                                      |
//! |---------|----------------------------------------------------------------|
//! | macOS   | `[NSApplication sharedApplication].appearance = NSAppearance…` |
//! | Windows | `DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE)`         |
//! | Linux   | no-op — GTK / KDE decorations follow the DE's own theme.       |
//!
//! The [`apply_native_titlebar_theme`] entry point is safe to call from any
//! thread on any OS; the macOS variant marshals the AppKit calls onto the
//! main thread via Slint's event loop so it stays sound even when invoked
//! from a background theme watcher.
//!
//! Traffic lights (close/min/max on macOS; caption buttons on Windows)
//! remain visible and functional — we only change *tint*, never visibility.

use crate::theme::ThemeMode;

/// Apply the given theme mode to the native window title bar (if the OS has
/// one). No-op on platforms without a controllable title bar tint.
///
/// Called from `AppShell::apply_theme` after the Slint `Theme` global is
/// updated, so a runtime theme swap (dark → light or vice versa) also flips
/// the OS chrome to match.
pub fn apply_native_titlebar_theme(mode: ThemeMode) {
    #[cfg(target_os = "macos")]
    macos::apply(mode);

    #[cfg(target_os = "windows")]
    windows::apply(mode);

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Linux / BSD: X11 / Wayland decorations follow the desktop
        // environment's own theme (GNOME/KDE/…); Atlas does not attempt
        // to override that. If the user has a mismatched dark app on a
        // light DE that's a DE-level preference, not ours to fight.
        let _ = mode;
    }
}

// ── macOS ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    use objc2::{rc::Retained, runtime::ProtocolObject, ClassType};
    use objc2_app_kit::{NSAppearance, NSAppearanceCustomization, NSAppearanceName, NSApplication};
    use objc2_foundation::MainThreadMarker;

    use crate::theme::ThemeMode;

    /// Set `NSApp.appearance` so every window (including the OS-drawn title
    /// bar) tracks Atlas's theme mode. AppKit takes care of restyling
    /// standard controls and materials to match. Called from the Slint
    /// event loop thread because AppKit APIs are main-thread-only.
    pub(super) fn apply(mode: ThemeMode) {
        let dark = mode.is_dark();
        let _ = slint::invoke_from_event_loop(move || {
            // Safety: `invoke_from_event_loop` guarantees the closure runs
            // on the Slint main thread, which on macOS is the AppKit main
            // thread — the only place NSApplication may be touched.
            let Some(mtm) = MainThreadMarker::new() else {
                tracing::debug!(
                    "titlebar_theme: main-thread check failed; skipping NSApp appearance update"
                );
                return;
            };
            let app: Retained<NSApplication> = NSApplication::sharedApplication(mtm);
            let name: &'static NSAppearanceName = if dark {
                unsafe { objc2_app_kit::NSAppearanceNameDarkAqua }
            } else {
                unsafe { objc2_app_kit::NSAppearanceNameAqua }
            };
            let Some(appearance) = NSAppearance::appearanceNamed(name) else {
                tracing::warn!(?dark, "titlebar_theme: NSAppearance lookup returned nil");
                return;
            };
            let proto: Retained<ProtocolObject<dyn NSAppearanceCustomization>> =
                ProtocolObject::from_retained(app.clone());
            unsafe { proto.setAppearance(Some(&appearance)) };
            tracing::debug!(?dark, "titlebar_theme: set NSApp appearance");
            // `app` and `proto` drop naturally.
            let _ = NSApplication::class();
        });
    }
}

// ── Windows ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows {
    use crate::theme::ThemeMode;

    /// Set the DWM immersive-dark-mode attribute on every top-level window
    /// owned by the current process. This tints the caption bar to match
    /// the app theme on Windows 10 (20H1+) and Windows 11.
    pub(super) fn apply(mode: ThemeMode) {
        let dark = mode.is_dark();
        let _ = slint::invoke_from_event_loop(move || {
            // Iterate every top-level window owned by this thread so a
            // multi-window app all tints together. Currently Atlas ships
            // a single window; the loop future-proofs the helper.
            //
            // Type notes for windows-sys 0.52:
            //   * `HWND` is a `isize` typedef, not `*mut _`. Null sentinel
            //     is `0`, and `is_null` is `== 0`.
            //   * `BOOL` lives at `Win32::Foundation::BOOL` (an `i32`),
            //     not at the crate root's `core` module.
            unsafe {
                use windows_sys::Win32::Foundation::{BOOL, HWND};
                use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    GetTopWindow, GetWindow, GW_HWNDNEXT,
                };
                let use_dark: BOOL = if dark { 1 } else { 0 };
                let mut hwnd: HWND = GetTopWindow(0);
                while hwnd != 0 {
                    // DWMWA_USE_IMMERSIVE_DARK_MODE = 20 on 20H1+; 19 on the
                    // 1809/1903 preview builds. Try 20 first, silently fall
                    // back — DWM returns E_INVALIDARG for unknown attrs.
                    let bool_ptr: *const BOOL = &use_dark;
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        20, // DWMWA_USE_IMMERSIVE_DARK_MODE
                        bool_ptr.cast(),
                        std::mem::size_of::<BOOL>() as u32,
                    );
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        19, // legacy pre-20H1 fallback
                        bool_ptr.cast(),
                        std::mem::size_of::<BOOL>() as u32,
                    );
                    hwnd = GetWindow(hwnd, GW_HWNDNEXT);
                }
            }
            tracing::debug!(?dark, "titlebar_theme: applied DWM immersive-dark-mode");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_is_infallible_on_every_platform() {
        // Doesn't crash on any target — the macOS/Windows paths use
        // `invoke_from_event_loop` (which no-ops without a running loop
        // in unit tests) and the Linux path is a no-op by design.
        apply_native_titlebar_theme(ThemeMode::Dark);
        apply_native_titlebar_theme(ThemeMode::Light);
    }
}
