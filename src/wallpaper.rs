// Ninimma modification: independent project branding and integration.
//! "Set as desktop background", like Explorer's. Each desktop keeps its own
//! setting: GNOME-family desktops through gsettings, KDE Plasma through
//! plasma-apply-wallpaperimage, Xfce through xfconf-query, macOS through
//! System Events, Windows through SystemParametersInfo. Tests only record
//! what would run, so they never change the real desktop.

use std::path::Path;

/// The commands that set `path` as the background on `desktop` (the
/// value of `XDG_CURRENT_DESKTOP`), or None for a desktop we don't know.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
pub fn commands(desktop: &str, path: &Path) -> Option<Vec<Vec<String>>> {
    let file = path.to_string_lossy().to_string();
    let uri = format!("file://{}", percent_encode(&file));
    let gsettings = |schema: &str, keys: &[&str], value: &str| -> Vec<Vec<String>> {
        keys.iter()
            .map(|key| {
                ["gsettings", "set", schema, key, value]
                    .map(String::from)
                    .to_vec()
            })
            .collect()
    };
    let desktops: Vec<String> = desktop.split(':').map(|d| d.to_ascii_lowercase()).collect();
    let is = |name: &str| desktops.iter().any(|d| d == name);
    Some(if is("cinnamon") || is("x-cinnamon") {
        gsettings("org.cinnamon.desktop.background", &["picture-uri"], &uri)
    } else if is("mate") {
        gsettings("org.mate.background", &["picture-filename"], &file)
    } else if is("kde") {
        vec![vec!["plasma-apply-wallpaperimage".into(), file]]
    } else if is("xfce") {
        // Every monitor and workspace Xfce knows about.
        vec![vec![
            "sh".into(),
            "-c".into(),
            "xfconf-query -c xfce4-desktop -l | grep '/last-image$' | \
             while read -r p; do xfconf-query -c xfce4-desktop -p \"$p\" -s \"$1\"; done"
                .into(),
            "sh".into(),
            file,
        ]]
    } else if is("gnome") || is("unity") || is("budgie") || is("zorin") || is("pop") || is("ubuntu")
    {
        // Both light and dark styles, or the dark one keeps the old picture.
        gsettings(
            "org.gnome.desktop.background",
            &["picture-uri", "picture-uri-dark"],
            &uri,
        )
    } else {
        return None;
    })
}

#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn percent_encode(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// What tests would have run.
#[cfg(test)]
pub static TEST_RAN: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Makes the picture at `path` the desktop background.
pub fn set(path: &Path) -> Result<(), String> {
    let path = std::fs::canonicalize(path).map_err(|e| format!("Couldn't find it: {e}"))?;
    #[cfg(test)]
    {
        TEST_RAN.lock().unwrap().push(path);
        Ok(())
    }
    #[cfg(not(test))]
    set_for_real(&path)
}

#[cfg(all(not(test), windows))]
fn set_for_real(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SPI_SETDESKWALLPAPER,
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            wide.as_mut_ptr().cast(),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        )
    };
    if ok == 0 {
        Err("Windows didn't accept that picture as the background".into())
    } else {
        Ok(())
    }
}

#[cfg(all(not(test), target_os = "macos"))]
fn set_for_real(path: &Path) -> Result<(), String> {
    let file = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let script = format!(
        "tell application \"System Events\" to tell every desktop to set picture to \"{file}\""
    );
    run(&["osascript".into(), "-e".into(), script])
}

#[cfg(all(not(test), not(windows), not(target_os = "macos")))]
fn set_for_real(path: &Path) -> Result<(), String> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let commands = commands(&desktop, path).ok_or_else(|| {
        format!(
            "Setting the background isn't supported on this desktop ({})",
            if desktop.is_empty() {
                "unknown"
            } else {
                desktop.as_str()
            }
        )
    })?;
    commands.iter().try_for_each(|c| run(c))
}

#[cfg(all(not(test), not(windows)))]
fn run(command: &[String]) -> Result<(), String> {
    let output = std::process::Command::new(&command[0])
        .args(&command[1..])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't run {}: {e}", command[0]))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} failed: {}",
            command[0],
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_desktop_gets_its_own_setting() {
        let p = Path::new("/home/me/Pictures/Grand Canyon.jpg");
        let gnome = commands("zorin:GNOME", p).unwrap();
        assert_eq!(gnome.len(), 2, "light and dark");
        assert_eq!(gnome[0][4], "file:///home/me/Pictures/Grand%20Canyon.jpg");
        assert_eq!(gnome[1][3], "picture-uri-dark");
        assert_eq!(
            commands("X-Cinnamon", p).unwrap()[0][2],
            "org.cinnamon.desktop.background"
        );
        assert_eq!(
            commands("KDE", p).unwrap()[0],
            [
                "plasma-apply-wallpaperimage",
                "/home/me/Pictures/Grand Canyon.jpg"
            ]
        );
        assert_eq!(
            commands("MATE", p).unwrap()[0][4],
            "/home/me/Pictures/Grand Canyon.jpg"
        );
        assert!(commands("XFCE", p).unwrap()[0].contains(&p.to_string_lossy().to_string()));
        assert!(commands("weird-wm", p).is_none());
    }
}
