//! macOS LaunchAgent installation helpers for atlas-indexd.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use directories::BaseDirs;

use crate::paths;

const LABEL: &str = "dev.atlas.atlas-indexd";

/// Install and load the atlas-indexd LaunchAgent.
pub fn install(socket: Option<PathBuf>) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = socket;
        bail!("launchd installation is only supported on macOS");
    }

    #[cfg(target_os = "macos")]
    {
        install_impl(socket)
    }
}

/// Unload and remove the atlas-indexd LaunchAgent.
pub fn uninstall() -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        bail!("launchd uninstallation is only supported on macOS");
    }

    #[cfg(target_os = "macos")]
    {
        uninstall_impl()
    }
}

#[cfg(target_os = "macos")]
fn install_impl(socket: Option<PathBuf>) -> Result<()> {
    let plist_path = plist_path()?;
    let base_dir = paths::base_dir()?;
    let logs_dir = paths::logs_dir()?;
    std::fs::create_dir_all(&base_dir)?;
    std::fs::create_dir_all(&logs_dir)?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let socket = socket.unwrap_or(paths::socket_path()?);
    let exe =
        std::env::current_exe().context("could not determine atlas-indexd executable path")?;
    let stdout_log = logs_dir.join("atlas-indexd.stdout.log");
    let stderr_log = logs_dir.join("atlas-indexd.stderr.log");
    let plist = plist_contents(&exe, &socket, &stdout_log, &stderr_log);
    std::fs::write(&plist_path, plist)
        .with_context(|| format!("write {}", plist_path.display()))?;

    let domain = launchd_domain()?;
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &plist_path.to_string_lossy()])
        .status();

    run_launchctl(["bootstrap", &domain, &plist_path.to_string_lossy()])?;
    run_launchctl(["enable", &format!("{domain}/{LABEL}")])?;
    run_launchctl(["kickstart", "-k", &format!("{domain}/{LABEL}")])?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_impl() -> Result<()> {
    let plist_path = plist_path()?;
    if plist_path.exists() {
        let domain = launchd_domain()?;
        let _ = Command::new("launchctl")
            .args(["bootout", &domain, &plist_path.to_string_lossy()])
            .status();
        std::fs::remove_file(&plist_path)
            .with_context(|| format!("remove {}", plist_path.display()))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn plist_contents(
    exe: &std::path::Path,
    socket: &std::path::Path,
    stdout_log: &std::path::Path,
    stderr_log: &std::path::Path,
) -> String {
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n",
            "<dict>\n",
            "  <key>Label</key>\n",
            "  <string>{label}</string>\n",
            "  <key>ProgramArguments</key>\n",
            "  <array>\n",
            "    <string>{exe}</string>\n",
            "    <string>--socket</string>\n",
            "    <string>{socket}</string>\n",
            "    <string>run</string>\n",
            "  </array>\n",
            "  <key>RunAtLoad</key>\n",
            "  <true/>\n",
            "  <key>KeepAlive</key>\n",
            "  <true/>\n",
            "  <key>StandardOutPath</key>\n",
            "  <string>{stdout}</string>\n",
            "  <key>StandardErrorPath</key>\n",
            "  <string>{stderr}</string>\n",
            "  <key>ProcessType</key>\n",
            "  <string>Background</string>\n",
            "</dict>\n",
            "</plist>\n"
        ),
        label = xml_escape(LABEL),
        exe = xml_escape(&exe.to_string_lossy()),
        socket = xml_escape(&socket.to_string_lossy()),
        stdout = xml_escape(&stdout_log.to_string_lossy()),
        stderr = xml_escape(&stderr_log.to_string_lossy()),
    )
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not determine home directory")?;
    Ok(base_dirs
        .home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to execute id -u")?;
    if !output.status.success() {
        bail!("id -u failed with status {}", output.status);
    }
    let uid = String::from_utf8(output.stdout)
        .context("id -u output was not utf-8")?
        .trim()
        .to_string();
    Ok(format!("gui/{uid}"))
}

#[cfg(target_os = "macos")]
fn run_launchctl<I, S>(args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let status = Command::new("launchctl")
        .args(args)
        .status()
        .context("failed to execute launchctl")?;
    if !status.success() {
        bail!("launchctl exited with status {status}");
    }
    Ok(())
}
