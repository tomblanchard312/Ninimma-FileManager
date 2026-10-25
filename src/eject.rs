// Ninimma modification: independent project branding and integration.
//! Eject (safely remove) a removable drive: `gio mount --eject` on Linux
//! and FreeBSD, `diskutil eject` on macOS, the shell's Eject verb on Windows.

use crate::file_system::{DriveInfo, DriveType};
use std::path::Path;
use std::process::Command;

/// Drives that can be ejected: USB sticks, card readers, discs.
pub fn ejectable(drive: &DriveInfo) -> bool {
    matches!(drive.drive_type, DriveType::Removable | DriveType::CDRom)
}

/// Lets tests substitute a scripted command for the platform's tool.
#[cfg(test)]
pub static TEST_TOOL: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Held by tests that set `TEST_TOOL`, so they don't swap it under each other.
#[cfg(test)]
pub static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn command(path: &Path) -> Command {
    #[cfg(test)]
    if let Some(tool) = TEST_TOOL.lock().unwrap().clone() {
        let mut c = Command::new(tool);
        c.arg(path);
        return c;
    }
    #[cfg(windows)]
    {
        // The Windows shell's Eject: flushes and powers the device down.
        let drive = crate::powershell::literal(&path.to_string_lossy());
        let mut c = Command::new("powershell");
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "(New-Object -ComObject Shell.Application).Namespace(17).ParseName({drive}).InvokeVerb('Eject')"
            ),
        ]);
        c
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = Command::new("diskutil");
        c.arg("eject").arg(path);
        c
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let mut c = Command::new("gio");
        c.args(["mount", "--eject"]).arg(path);
        c
    }
}

/// Ejects the drive mounted at `path`. Fails (with the tool's reason) when
/// something still has files open on it.
pub fn eject(path: &Path) -> Result<(), String> {
    let output = command(path)
        .output()
        .map_err(|e| format!("Couldn't run the eject tool: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let reason = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if reason.is_empty() {
        "Couldn't eject it. Close any files open on it and try again.".to_string()
    } else {
        format!("Couldn't eject it: {reason}")
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn reports_why_a_drive_is_busy() {
        let _serial = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("eject");
        std::fs::write(&tool, "#!/bin/sh\necho 'target is busy' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        *TEST_TOOL.lock().unwrap() = Some(tool.to_string_lossy().to_string());
        let err = eject(Path::new("/media/usb")).unwrap_err();
        *TEST_TOOL.lock().unwrap() = None;
        assert!(err.contains("target is busy"), "{err}");
    }
}
