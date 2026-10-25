// Ninimma modification: independent project branding and integration.
//! Disk operations run in an OS terminal so elevation and encryption secrets
//! stay with the operating system, rather than the app's settings or argv.
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Mount,
    Unmount,
    Unlock,
    Lock,
}

impl Operation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mount => "Mount VHD/VHDX",
            Self::Unmount => "Unmount VHD/VHDX",
            Self::Unlock => "Unlock BitLocker",
            Self::Lock => "Lock / unmount BitLocker",
        }
    }

    pub fn is_image(self) -> bool {
        matches!(self, Self::Mount | Self::Unmount)
    }

    #[cfg(target_os = "linux")]
    fn argument(self) -> &'static str {
        match self {
            Self::Mount => "mount",
            Self::Unmount => "unmount",
            Self::Unlock => "unlock",
            Self::Lock => "lock",
        }
    }
}

pub const SUPPORTED: bool = cfg!(any(windows, target_os = "linux"));

pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("vhd") || s.eq_ignore_ascii_case("vhdx"))
}

pub fn launch(
    operation: Operation,
    target: &str,
    read_only: bool,
    recovery_password: bool,
) -> Result<(), String> {
    let target = target.trim();
    if target.is_empty() || target.contains(['\n', '\r', '\0']) {
        return Err("Enter a disk image path or volume.".into());
    }
    if operation.is_image() && !is_image(Path::new(target)) {
        return Err("Choose a .vhd or .vhdx disk image.".into());
    }
    #[cfg(target_os = "linux")]
    {
        let _ = recovery_password; // cryptsetup accepts either at its hidden prompt.
        launch_linux(operation, target, read_only)
    }
    #[cfg(windows)]
    {
        launch_windows(operation, target, read_only, recovery_password)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (operation, target, read_only, recovery_password);
        Err("Disk images and BitLocker are supported on Windows and Linux.".into())
    }
}

#[cfg(target_os = "linux")]
fn launch_linux(operation: Operation, target: &str, read_only: bool) -> Result<(), String> {
    // No user text is interpolated into shell source. It follows the fixed
    // wrapper as separate argv entries, including image paths with spaces.
    let wrapper = r#""$@"; result=$?; printf '\nOperation exited with status %s. Press Enter to close.\n' "$result"; read -r answer; exit "$result""#;
    let payload = [
        "bash",
        "-c",
        wrapper,
        "ninimma-disks",
        "sudo",
        "--",
        "python3",
        "-c",
        include_str!("../packaging/linux/disk_images.py"),
        operation.argument(),
        target,
        if read_only { "ro" } else { "rw" },
    ];
    for (terminal, prefix) in [
        ("x-terminal-emulator", vec!["-e"]),
        ("gnome-terminal", vec!["--"]),
        ("konsole", vec!["-e"]),
        ("kgx", vec!["--"]),
        ("ptyxis", vec!["--"]),
        ("xfce4-terminal", vec!["--execute"]),
        ("xterm", vec!["-e"]),
    ] {
        let Some(mut command) = crate::terminal::command(terminal, None)? else {
            continue;
        };
        match command.args(prefix).args(payload).spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("Could not start {terminal}: {e}")),
        }
    }
    Err("Install a terminal on the host (x-terminal-emulator, gnome-terminal, konsole, kgx, ptyxis, xfce4-terminal or xterm) to perform disk operations.".into())
}

#[cfg(any(windows, test))]
fn windows_script(op: Operation, target: &str, read_only: bool, recovery: bool) -> String {
    let target = crate::powershell::literal(target);
    let action = match op {
        Operation::Mount => format!(
            "Mount-DiskImage -ImagePath {target} -Access {} -ErrorAction Stop | Out-Null; Get-DiskImage -ImagePath {target} | Get-Disk | Get-Partition | Format-Table PartitionNumber, DriveLetter, Size",
            if read_only { "ReadOnly" } else { "ReadWrite" }
        ),
        Operation::Unmount => format!("Dismount-DiskImage -ImagePath {target} -ErrorAction Stop"),
        Operation::Unlock if recovery => format!(
            "$secret = Read-Host '48-digit recovery password' -AsSecureString; $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secret); try {{ Unlock-BitLocker -MountPoint {target} -RecoveryPassword ([Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)) -ErrorAction Stop | Out-Null }} finally {{ [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr); $secret.Dispose() }}"
        ),
        Operation::Unlock => format!(
            "$secret = Read-Host 'BitLocker password' -AsSecureString; try {{ Unlock-BitLocker -MountPoint {target} -Password $secret -ErrorAction Stop | Out-Null }} finally {{ $secret.Dispose() }}"
        ),
        Operation::Lock => format!("Lock-BitLocker -MountPoint {target} -ErrorAction Stop | Out-Null"),
    };
    format!("$ErrorActionPreference = 'Stop'; try {{ {action}; Write-Host 'Operation completed.' }} catch {{ Write-Host $_.Exception.Message -ForegroundColor Red }}; Read-Host 'Press Enter to close' | Out-Null")
}

// Windows PowerShell's -EncodedCommand takes UTF-16LE in base64. Encoding the
// complete script avoids an extra command-line quoting layer during UAC.
#[cfg(any(windows, test))]
fn encoded_command(script: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(windows)]
fn launch_windows(
    op: Operation,
    target: &str,
    read_only: bool,
    recovery: bool,
) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let script = encoded_command(&windows_script(op, target, read_only, recovery));
    let launcher = format!(
        "$ErrorActionPreference = 'Stop'; Start-Process -FilePath \"$env:SystemRoot\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -Verb RunAs -ArgumentList @('-NoProfile', '-EncodedCommand', '{script}')"
    );
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &launcher])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW: the elevated console owns the prompts.
        .status()
        .map_err(|e| format!("Could not start Windows PowerShell: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("The disk operation could not start. Accept the Windows administrator prompt to continue.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_disk_images_only() {
        assert!(is_image(Path::new("archive.VHDX")));
        assert!(is_image(Path::new("disk.vhd")));
        assert!(!is_image(Path::new("disk.vhd.exe")));
    }

    #[test]
    fn powershell_paths_cannot_become_commands() {
        let script = windows_script(
            Operation::Mount,
            "C:\\a'; Write-Host injected; #.vhdx",
            true,
            false,
        );
        assert!(script.contains("-ImagePath 'C:\\a''; Write-Host injected; #.vhdx'"));
        assert!(script.contains("-Access ReadOnly"));
        let script = windows_script(
            Operation::Mount,
            "C:\\a\u{2019}; Write-Host injected; #.vhdx",
            true,
            false,
        );
        assert!(script.contains("-ImagePath 'C:\\a\u{2019}\u{2019}; Write-Host injected; #.vhdx'"));
        assert!(!script.contains("-Access ReadWrite"));
    }

    #[test]
    fn credentials_are_prompted_and_locks_do_not_force_dismount() {
        for recovery in [false, true] {
            let script = windows_script(Operation::Unlock, "E:", true, recovery);
            assert!(script.contains("-AsSecureString"));
            assert!(script.contains("$secret.Dispose()"));
        }
        assert!(!windows_script(Operation::Lock, "E:", true, false).contains("ForceDismount"));
    }

    #[test]
    fn encodes_utf16_for_windows_powershell() {
        assert_eq!(encoded_command("A"), "QQA=");
        assert_eq!(encoded_command("AB"), "QQBCAA==");
        assert_eq!(encoded_command("ABC"), "QQBCAEMA");
    }
}
