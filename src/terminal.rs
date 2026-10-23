//! Launch desktop terminals on the host when running inside Flatpak.
use std::{path::Path, process::Command};

// The executable name is an argument, never shell source.
const HOST_PROBE: &str = "command -v \"$1\" >/dev/null || exit 44";

pub fn command(program: &str, directory: Option<&Path>) -> Result<Option<Command>, String> {
    let mut command = if cfg!(target_os = "linux") && std::env::var_os("FLATPAK_ID").is_some() {
        let probe = Command::new("flatpak-spawn")
            .args(["--host", "sh", "-c", HOST_PROBE, "terminal-probe", program])
            .output()
            .map_err(|e| format!("Could not access host terminals through Flatpak: {e}"))?;
        if probe.status.code() == Some(44) {
            return Ok(None);
        }
        if !probe.status.success() {
            return Err(format!(
                "Could not access host terminals through Flatpak: {}",
                String::from_utf8_lossy(&probe.stderr).trim()
            ));
        }
        let mut command = Command::new("flatpak-spawn");
        command.arg("--host");
        if let Some(directory) = directory {
            let mut argument = std::ffi::OsString::from("--directory=");
            argument.push(directory);
            command.arg(argument);
        }
        command.arg(program);
        command
    } else {
        Command::new(program)
    };
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    Ok(Some(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_probe_distinguishes_missing_programs_without_interpreting_them() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("injected");
        for (program, expected) in [
            ("sh".to_string(), 0),
            (format!("sh; touch '{}'", marker.display()), 44),
        ] {
            let output = Command::new("sh")
                .args(["-c", HOST_PROBE, "terminal-probe", &program])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(expected));
        }
        assert!(!marker.exists());
    }

    #[test]
    fn native_commands_keep_the_requested_working_directory() {
        if std::env::var_os("FLATPAK_ID").is_some() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let output = command("sh", Some(dir.path()))
            .unwrap()
            .unwrap()
            .args(["-c", "pwd -P"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            dir.path().canonicalize().unwrap().to_str().unwrap()
        );
    }

    #[test]
    #[ignore = "opens a host terminal; run inside the installed Flatpak explicitly"]
    fn flatpak_opens_host_terminal_and_preserves_arguments() {
        assert!(std::env::var_os("FLATPAK_ID").is_some());
        let cache = std::env::var_os("XDG_CACHE_HOME").unwrap();
        assert!(command("ninimma-missing-terminal-test", None)
            .unwrap()
            .is_none());
        let dir = tempfile::Builder::new()
            .prefix("terminal bridge ")
            .tempdir_in(cache)
            .unwrap();
        let marker = dir.path().join("terminal result.txt");
        let argument = "image with spaces; $(not-a-command) 'quoted'.vhdx";
        let mut terminal = command("x-terminal-emulator", Some(dir.path()))
            .unwrap()
            .unwrap();
        let status = terminal
            .args([
                "-e",
                "sh",
                "-c",
                "printf '%s' \"$1\" > \"$2\"",
                "terminal-test",
                argument,
            ])
            .arg(marker.file_name().unwrap())
            .status()
            .unwrap();
        assert!(status.success());
        for _ in 0..100 {
            if std::fs::read_to_string(&marker).ok().as_deref() == Some(argument) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("host terminal did not execute the payload");
    }
}
