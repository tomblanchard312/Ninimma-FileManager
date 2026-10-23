// Ninimma modification: opt-in folder association with restoration.
use std::{fs, path::Path, process::Command};

const DESKTOP: &str = "ninimma.desktop";

pub fn change(restore: bool) -> Result<String, String> {
    if std::env::var_os("FLATPAK_ID").is_some() {
        return Err(
            "Use your desktop's Default Applications settings to select the Flatpak edition."
                .into(),
        );
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".config")))
        .ok_or("Cannot find your configuration folder")?;
    update(
        &config.join("ninimma/previous-folder-app"),
        restore,
        |args| {
            let output = Command::new("xdg-mime")
                .args(args)
                .output()
                .map_err(|e| format!("Cannot run xdg-mime (install xdg-utils): {e}"))?;
            if !output.status.success() {
                return Err(format!(
                    "xdg-mime failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        },
    )
}

fn update(
    backup: &Path,
    restore: bool,
    mut run: impl FnMut(&[&str]) -> Result<String, String>,
) -> Result<String, String> {
    let current = run(&["query", "default", "inode/directory"])?;
    if restore {
        if current != DESKTOP {
            return Err("Your default has changed outside Ninimma; it was left unchanged.".into());
        }
        let previous =
            fs::read_to_string(backup).map_err(|e| format!("Cannot read previous default: {e}"))?;
        if !valid_desktop(&previous) || previous == DESKTOP {
            return Err("The saved default is invalid; select one in Default Applications.".into());
        }
        run(&["default", &previous, "inode/directory"])?;
        // Retain the backup if the desktop reports that the change did not stick.
        if run(&["query", "default", "inode/directory"])? != previous {
            return Err("The desktop did not restore the previous default.".into());
        }
        fs::remove_file(backup)
            .map_err(|e| format!("Default restored, but cannot remove backup: {e}"))?;
        Ok("Previous folder application restored.".into())
    } else {
        if current == DESKTOP {
            return Ok("Ninimma is already your default folder application.".into());
        }
        if !valid_desktop(&current) {
            return Err(
                "No existing folder default was found to save. Use Default Applications instead."
                    .into(),
            );
        }
        fs::create_dir_all(backup.parent().ok_or("Invalid backup path")?)
            .map_err(|e| e.to_string())?;
        fs::write(backup, &current).map_err(|e| format!("Cannot save previous default: {e}"))?;
        run(&["default", DESKTOP, "inode/directory"])?;
        if run(&["query", "default", "inode/directory"])? != DESKTOP {
            return Err("The desktop did not select Ninimma as the default.".into());
        }
        Ok("Ninimma is now your default folder application.".into())
    }
}

fn valid_desktop(value: &str) -> bool {
    value.ends_with(".desktop") && !value.contains(['\n', '\r', '/', '\\'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_is_idempotent_and_restores_the_actual_previous_app() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("ninimma/previous");
        let mut current = "org.gnome.Nautilus.desktop".to_owned();
        let mut run = |args: &[&str]| {
            if args[0] == "default" {
                current = args[1].into();
            }
            Ok(current.clone())
        };
        update(&backup, false, &mut run).unwrap();
        update(&backup, false, &mut run).unwrap();
        assert_eq!(
            fs::read_to_string(&backup).unwrap(),
            "org.gnome.Nautilus.desktop"
        );
        update(&backup, true, &mut run).unwrap();
        assert_eq!(current, "org.gnome.Nautilus.desktop");
        assert!(!backup.exists());
    }

    #[test]
    fn failed_set_preserves_backup() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("previous");
        let result = update(&backup, false, |args| {
            if args[0] == "default" {
                Err("failed".into())
            } else {
                Ok("nemo.desktop".into())
            }
        });
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(backup).unwrap(), "nemo.desktop");
    }

    #[test]
    fn restore_does_not_overwrite_a_new_external_default() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("previous");
        fs::write(&backup, "nemo.desktop").unwrap();
        assert!(update(&backup, true, |args| {
            assert_eq!(args[0], "query");
            Ok("thunar.desktop".into())
        })
        .is_err());
        assert!(backup.exists());
    }

    #[test]
    fn missing_default_does_not_change_associations() {
        let dir = tempfile::tempdir().unwrap();
        assert!(update(&dir.path().join("previous"), false, |args| {
            assert_eq!(args[0], "query");
            Ok(String::new())
        })
        .is_err());
    }
}
