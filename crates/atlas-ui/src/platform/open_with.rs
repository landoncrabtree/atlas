//! Native **"Open With…"** application picker per platform.
//!
//! Right-click → *Open With…* is semantically distinct from *Open* — the
//! user wants to pick an application that isn't the OS default. Atlas
//! shells out to the platform-native picker rather than shipping its own
//! app chooser, so the experience matches what users see in Finder /
//! Explorer / their DE's file manager.
//!
//! # Platform matrix
//!
//! | OS      | Mechanism                                                            |
//! |---------|----------------------------------------------------------------------|
//! | macOS   | AppleScript's `choose application` dialog via `osascript`, then      |
//! |         | `/usr/bin/open -a <selected-app.app> <file>`                         |
//! | Windows | `Rundll32.exe Shell32.dll,OpenAs_RunDLL <absolute-path>` — the built- |
//! |         | in *Open With* dialog                                                |
//! | Linux   | `mimeopen -a <path>` (from `perl-file-mimeinfo`) if present on PATH; |
//! |         | otherwise falls back to `xdg-open` with a `tracing::warn!` explaining|
//! |         | that no native picker CLI was found                                  |
//!
//! # Why not `open -a "" <file>` on macOS?
//!
//! On modern macOS (Sonoma / Sequoia / Tahoe) the built-in `open(1)` has
//! no flag that pops the "Choose Application" chooser. Passing an empty
//! app name to `-a` prints `Unable to find application named ''` and
//! exits non-zero. The pragmatic non-FFI approach is the AppleScript
//! `choose application` command — it invokes the exact same Launch
//! Services-backed dialog Finder uses for *Open With → Other…*, complete
//! with the icon grid and the *All Applications* toggle. Once the user
//! picks (or cancels), we hand the resulting bundle path to `open -a`.
//!
//! # Threading
//!
//! [`open_with_picker`] is **blocking** — it waits for the user to make
//! a choice. Callers on the Slint UI thread must spawn a worker thread
//! (see `crates/atlas-ui/src/shell.rs::on_ctx_open_with`) so the event
//! loop keeps running while the picker is up.
//!
//! # Local-only
//!
//! The picker always operates on a filesystem path the OS can resolve.
//! Remote entries must be downloaded through the preview cache first
//! (see `crate::remote::preview`); callers guarding via
//! [`atlas_core::Location::as_local`] is enforced by the capability
//! resolver (`can_open_with = is_local`).

use std::path::{Path, PathBuf};
use std::process::ExitStatus;

/// Errors produced by [`open_with_picker`].
///
/// Auth / permission failures inside the launched application (e.g. the
/// picked app refuses to open the file for its own reasons) are not
/// surfaced — once `open`/`Rundll32`/`mimeopen` hands the file off, we
/// consider the picker flow complete.
#[derive(Debug, thiserror::Error)]
pub enum OpenWithError {
    /// The path does not exist on disk. Guards every OS-specific branch
    /// before spawning so we fail fast with a clear message instead of
    /// waiting for the picker to reject an unknown file.
    #[error("path does not exist: {0}")]
    NotFound(PathBuf),

    /// The user dismissed the picker without choosing an application.
    /// This is not a bug — the caller should log at debug and move on.
    #[error("user cancelled the Open With picker")]
    UserCancelled,

    /// Failed to spawn the picker binary (missing `osascript`,
    /// `Rundll32`, or `mimeopen` / `xdg-open` on PATH).
    #[error("failed to spawn {program}: {source}")]
    Spawn {
        /// The command we tried to spawn (`osascript`, `Rundll32.exe`,
        /// `mimeopen`, or `xdg-open`).
        program: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The picker exited non-zero for some other reason (I/O error,
    /// AppleScript syntax problem, application launch failure).
    #[error("{program} exited with status {status}: {stderr}")]
    PickerFailed {
        /// Which picker binary failed.
        program: String,
        /// Its exit status.
        status: ExitStatus,
        /// Captured stderr (trimmed; empty if the child produced none).
        stderr: String,
    },
}

/// Show the platform-native *Open With…* application picker for `path`.
///
/// Blocks until the user picks an application (which is then invoked
/// with `path`) or cancels. On success returns `Ok(())`; cancellation
/// returns [`OpenWithError::UserCancelled`] — treat that as informational,
/// not an error the user should see a toast for.
///
/// # Errors
///
/// * [`OpenWithError::NotFound`] — `path` does not exist. The picker
///   is not spawned.
/// * [`OpenWithError::UserCancelled`] — the user dismissed the picker.
/// * [`OpenWithError::Spawn`] — the picker binary could not be launched
///   (e.g. `osascript` missing on macOS, `Rundll32.exe` missing on
///   Windows, neither `mimeopen` nor `xdg-open` on Linux PATH).
/// * [`OpenWithError::PickerFailed`] — the picker binary ran but
///   exited non-zero for a reason other than user cancellation.
///
/// # Blocking
///
/// This function blocks the calling thread for the entire duration the
/// picker dialog is on screen. Callers on any UI thread must spawn a
/// worker thread first — see `crate::shell::AppShell::on_ctx_open_with`.
pub fn open_with_picker(path: &Path) -> Result<(), OpenWithError> {
    if !path.exists() {
        return Err(OpenWithError::NotFound(path.to_path_buf()));
    }

    #[cfg(target_os = "macos")]
    {
        macos::run(path)
    }

    #[cfg(target_os = "windows")]
    {
        windows::run(path)
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux::run(path)
    }
}

// ── macOS ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    use std::path::Path;
    use std::process::Command;

    use super::OpenWithError;

    /// Two-step flow:
    ///
    /// 1. `osascript -e 'POSIX path of (choose application …)'` — pops
    ///    the native macOS *Choose Application* dialog. On success stdout
    ///    is the selected `.app` bundle's POSIX path (e.g.
    ///    `/System/Applications/TextEdit.app/`). On user-cancel osascript
    ///    exits **1** with `execution error: User canceled. (-128)` on
    ///    stderr — we map that to [`OpenWithError::UserCancelled`].
    /// 2. `/usr/bin/open -a <selected-app> <file>` — hand the file to
    ///    the chosen application via Launch Services.
    ///
    /// Splitting the two calls (instead of a monolithic `tell "Finder"
    /// to open POSIX file … using …` script) keeps error handling
    /// legible and avoids `Finder` being *made* the launching parent —
    /// AppleScript's `open using` under Finder attributes the launch to
    /// Finder, which slightly changes app-activation ordering versus a
    /// direct `open -a`.
    pub(super) fn run(path: &Path) -> Result<(), OpenWithError> {
        let script = format!(
            "POSIX path of (choose application with prompt \"{prompt}\" as alias)",
            prompt = choose_app_prompt(path),
        );
        let output = Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map_err(|source| OpenWithError::Spawn {
                program: "osascript".to_owned(),
                source,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if stderr.contains("User canceled") || stderr.contains("(-128)") {
                return Err(OpenWithError::UserCancelled);
            }
            return Err(OpenWithError::PickerFailed {
                program: "osascript".to_owned(),
                status: output.status,
                stderr,
            });
        }
        let app_path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if app_path.is_empty() {
            return Err(OpenWithError::PickerFailed {
                program: "osascript".to_owned(),
                status: output.status,
                stderr: "empty application path".to_owned(),
            });
        }

        let mut open_cmd = build_open_command(&app_path, path);
        let status = open_cmd.status().map_err(|source| OpenWithError::Spawn {
            program: "open".to_owned(),
            source,
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(OpenWithError::PickerFailed {
                program: "open".to_owned(),
                status,
                stderr: String::new(),
            })
        }
    }

    /// Build the second-stage `/usr/bin/open -a <app> <file>` invocation.
    ///
    /// Extracted for unit tests to assert the argv shape without
    /// spawning a real process.
    pub(super) fn build_open_command(app_path: &str, target: &Path) -> Command {
        let mut cmd = Command::new("/usr/bin/open");
        cmd.arg("-a").arg(app_path).arg(target);
        cmd
    }

    /// The single-quoted prompt for the AppleScript `choose application`
    /// call. AppleScript double-quotes need to be escaped; the filename
    /// is only used for display, so any weird characters do not affect
    /// what actually gets opened.
    fn choose_app_prompt(path: &Path) -> String {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        format!("Open “{escaped}” with:")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn choose_app_prompt_uses_filename() {
            let p = Path::new("/tmp/example.txt");
            let prompt = choose_app_prompt(p);
            assert!(prompt.contains("example.txt"), "prompt={prompt}");
            assert!(prompt.starts_with("Open "), "prompt={prompt}");
        }

        #[test]
        fn choose_app_prompt_escapes_double_quotes() {
            let p = Path::new("/tmp/weird\"name.txt");
            let prompt = choose_app_prompt(p);
            assert!(prompt.contains("\\\""), "prompt={prompt}");
        }

        #[test]
        fn build_open_command_uses_dash_a_and_absolute_path() {
            let cmd = build_open_command("/Applications/TextEdit.app", Path::new("/tmp/hello.txt"));
            assert_eq!(cmd.get_program(), "/usr/bin/open");
            let args: Vec<_> = cmd.get_args().collect();
            assert_eq!(args.len(), 3);
            assert_eq!(args[0], "-a");
            assert_eq!(args[1], "/Applications/TextEdit.app");
            assert_eq!(args[2], "/tmp/hello.txt");
        }
    }
}

// ── Windows ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows {
    use std::path::Path;
    use std::process::Command;

    use super::OpenWithError;

    /// `Rundll32.exe Shell32.dll,OpenAs_RunDLL <path>` invokes the
    /// built-in Windows *Open With…* dialog. The argument **must** be
    /// an absolute path — the OpenAs shell handler does not resolve
    /// relative paths against the caller's cwd.
    ///
    /// User-cancel produces exit code 0 (Rundll32 always exits 0 as
    /// long as the entry point was found), so we can't distinguish
    /// "user cancelled" from "app launched successfully". Both are
    /// mapped to `Ok(())` here — the caller sees no toast either way.
    pub(super) fn run(path: &Path) -> Result<(), OpenWithError> {
        let abs = path.canonicalize().map_err(|source| OpenWithError::Spawn {
            program: "canonicalize".to_owned(),
            source,
        })?;
        let mut cmd = build_command(&abs);
        let status = cmd.status().map_err(|source| OpenWithError::Spawn {
            program: "Rundll32.exe".to_owned(),
            source,
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(OpenWithError::PickerFailed {
                program: "Rundll32.exe".to_owned(),
                status,
                stderr: String::new(),
            })
        }
    }

    /// Extracted argv builder for unit tests.
    pub(super) fn build_command(path: &Path) -> Command {
        let mut cmd = Command::new("Rundll32.exe");
        cmd.arg("Shell32.dll,OpenAs_RunDLL").arg(path);
        cmd
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn build_command_uses_rundll_openas_verb() {
            let cmd = build_command(Path::new(r"C:\Users\Alice\file.txt"));
            assert_eq!(cmd.get_program(), "Rundll32.exe");
            let args: Vec<_> = cmd.get_args().collect();
            assert_eq!(args.len(), 2);
            assert_eq!(args[0], "Shell32.dll,OpenAs_RunDLL");
            assert_eq!(args[1], r"C:\Users\Alice\file.txt");
        }
    }
}

// ── Linux / other Unix ───────────────────────────────────────────────────────

#[cfg(all(unix, not(target_os = "macos")))]
mod linux {
    use std::path::Path;
    use std::process::Command;

    use super::OpenWithError;

    /// Try `mimeopen -a <path>` (from `perl-file-mimeinfo`, packaged as
    /// `perl-file-mimeinfo` on Ubuntu / Debian) — this is the closest
    /// thing to a DE-agnostic picker. When it isn't installed we fall
    /// back to `xdg-open` (which uses the default handler, not a picker)
    /// with a `tracing::warn!` so the user understands why they didn't
    /// see a chooser.
    ///
    /// KDE-specific `kdialog --openwith` and GNOME's Nautilus DBus
    /// interface would give a better UX per DE but require detecting
    /// the running DE at runtime (checking `XDG_CURRENT_DESKTOP` etc.),
    /// which is out of scope for this handler — the deferred work is
    /// tracked in the module docstring.
    pub(super) fn run(path: &Path) -> Result<(), OpenWithError> {
        match build_mimeopen_command(path).status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                tracing::warn!(
                    ?path,
                    ?status,
                    "open_with: mimeopen exited non-zero; falling back to xdg-open"
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    ?path,
                    "open_with: mimeopen not on PATH; falling back to xdg-open (default handler)"
                );
            }
            Err(source) => {
                return Err(OpenWithError::Spawn {
                    program: "mimeopen".to_owned(),
                    source,
                });
            }
        }

        let mut cmd = build_xdg_open_command(path);
        let status = cmd.status().map_err(|source| OpenWithError::Spawn {
            program: "xdg-open".to_owned(),
            source,
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(OpenWithError::PickerFailed {
                program: "xdg-open".to_owned(),
                status,
                stderr: String::new(),
            })
        }
    }

    /// Extracted argv builder for unit tests.
    pub(super) fn build_mimeopen_command(path: &Path) -> Command {
        let mut cmd = Command::new("mimeopen");
        cmd.arg("-a").arg(path);
        cmd
    }

    /// Extracted argv builder for unit tests.
    pub(super) fn build_xdg_open_command(path: &Path) -> Command {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(path);
        cmd
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn build_mimeopen_command_uses_dash_a() {
            let cmd = build_mimeopen_command(Path::new("/tmp/hello.txt"));
            assert_eq!(cmd.get_program(), "mimeopen");
            let args: Vec<_> = cmd.get_args().collect();
            assert_eq!(args.len(), 2);
            assert_eq!(args[0], "-a");
            assert_eq!(args[1], "/tmp/hello.txt");
        }

        #[test]
        fn build_xdg_open_command_passes_path_only() {
            let cmd = build_xdg_open_command(Path::new("/tmp/hello.txt"));
            assert_eq!(cmd.get_program(), "xdg-open");
            let args: Vec<_> = cmd.get_args().collect();
            assert_eq!(args.len(), 1);
            assert_eq!(args[0], "/tmp/hello.txt");
        }
    }
}

// ── Cross-platform tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_with_picker_returns_not_found_for_missing_path() {
        // Pick a path that reliably does not exist without touching
        // `/tmp` (see workspace hygiene rules).
        let missing = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("target")
            .join("__atlas_open_with_missing_probe__.dat");
        // Sanity: even if a stray file with this name exists (extremely
        // unlikely), the assertion below still holds when we prove the
        // path does not exist first.
        if !missing.exists() {
            let err = open_with_picker(&missing).expect_err("missing path must error");
            match err {
                OpenWithError::NotFound(p) => assert_eq!(p, missing),
                other => panic!("expected NotFound, got {other:?}"),
            }
        }
    }
}
