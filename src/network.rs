// Ninimma modification: independent project branding and integration.
//! Network shares and cloud storage: discovering what's connected,
//! connecting to `smb://`, `sftp://`… addresses, and disconnecting.
//!
//! Cloud services and network shares appear as ordinary folders through GVfs,
//! rclone, kernel mounts or sync clients. This module discovers and labels them.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Shares and devices are only discovered on Linux so far.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub enum PlaceKind {
    /// A network share (SMB, NFS, SFTP, WebDAV, FTP…).
    Network,
    /// Cloud storage that streams files on demand (rclone, GNOME Online
    /// Accounts). Reading a file downloads it.
    CloudStream,
    /// A cloud sync folder whose files are kept on this computer.
    CloudSync,
    /// A phone or camera (MTP / PTP).
    Device,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub label: String,
    pub path: PathBuf,
    pub kind: PlaceKind,
    /// How to disconnect it, if the user may.
    pub unmount: Option<Unmount>,
    /// Shown as a tooltip, e.g. why a place behaves oddly.
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub enum Unmount {
    /// GVfs location (`gio mount -u`).
    Gio,
    /// User FUSE mount (`fusermount -u`).
    Fuse,
}

/// Everything network/cloud currently available. Fairly cheap, but may touch
/// slow mounts, so call it from a background thread.
pub fn discover() -> Vec<Place> {
    let mut places = Vec::new();

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mounts = crate::file_system::mount_table();
        places.extend(places_from_mounts(&mounts));
        // Shares opened in KDE apps, exposed to other apps by kio-fuse.
        {
            for line in mounts
                .lines()
                .filter(|l| l.split_whitespace().nth(2) == Some("fuse.kio-fuse"))
            {
                if let Some(root) = line.split_whitespace().nth(1) {
                    places.extend(kio_fuse_places(Path::new(&unescape_mount(root))));
                }
            }
        }
        if let Some(gvfs) = gvfs_dir() {
            if let Ok(entries) = std::fs::read_dir(&gvfs) {
                let mut found: Vec<Place> = entries
                    .flatten()
                    .filter_map(|e| {
                        place_from_gvfs_name(&e.file_name().to_string_lossy(), e.path())
                    })
                    .collect();
                found.sort_by(|a, b| a.label.cmp(&b.label));
                places.extend(found);
            }
        }
    }

    #[cfg(windows)]
    {
        for var in ["OneDriveCommercial", "OneDriveConsumer", "OneDrive"] {
            if let Some(p) = std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
            {
                if !places.iter().any(|pl: &Place| pl.path == p) {
                    let label = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "OneDrive".into());
                    places.push(Place {
                        label,
                        path: p,
                        kind: PlaceKind::CloudSync,
                        unmount: None,
                        note: None,
                    });
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = crate::file_system::home_dir() {
        // OneDrive, Google Drive, Dropbox and iCloud's File Provider folders.
        if let Ok(entries) = std::fs::read_dir(home.join("Library/CloudStorage")) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().replace('-', " ");
                places.push(Place {
                    label: name,
                    path: e.path(),
                    kind: PlaceKind::CloudSync,
                    unmount: None,
                    note: None,
                });
            }
        }
    }

    // Sync clients that keep a folder in the home directory.
    if let Some(home) = crate::file_system::home_dir() {
        for name in [
            "Dropbox",
            "OneDrive",
            "Google Drive",
            "My Drive",
            "pCloudDrive",
            "Nextcloud",
            "ownCloud",
            "MEGA",
            "Box",
            "Insync",
        ] {
            let p = home.join(name);
            if p.is_dir() && !places.iter().any(|pl| pl.path == p) {
                places.push(Place {
                    label: name.to_string(),
                    path: p,
                    kind: PlaceKind::CloudSync,
                    unmount: None,
                    note: None,
                });
            }
        }
    }
    places
}

#[cfg(all(unix, not(target_os = "macos")))]
fn gvfs_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
        .join("gvfs");
    dir.is_dir().then_some(dir)
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
/// Turn a GVfs mount directory name such as
/// `smb-share:server=nas,share=photos` into a place.
pub fn place_from_gvfs_name(name: &str, path: PathBuf) -> Option<Place> {
    let (scheme, rest) = name.split_once(':')?;
    let field = |key: &str| {
        rest.split(',')
            .find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .map(percent_unescape)
    };
    let host = field("host")
        .or_else(|| field("server"))
        .unwrap_or_default();
    let user = field("user");
    let (label, kind, note) = match scheme {
        "smb-share" => (
            format!("{} on {}", field("share").unwrap_or_default(), host),
            PlaceKind::Network,
            None,
        ),
        "smb-server" => (host.clone(), PlaceKind::Network, None),
        "sftp" | "ftp" | "ftps" | "dav" | "davs" | "afp-volume" | "nfs" => {
            let what = field("volume").unwrap_or_else(|| host.clone());
            let label = match &user {
                Some(u) if scheme == "sftp" => format!("{u}@{what}"),
                _ => what,
            };
            (label, PlaceKind::Network, None)
        }
        "google-drive" | "onedrive" => {
            let service = if scheme == "google-drive" {
                "Google Drive"
            } else {
                "OneDrive"
            };
            let account = match &user {
                Some(u) => format!("{u}@{host}"),
                None => host.clone(),
            };
            let note = (scheme == "google-drive").then(|| {
                "Connected through GNOME Online Accounts, which shows folders by internal IDs to \
                 apps other than GNOME's own. For real folder names, connect with rclone instead."
                    .to_string()
            });
            (
                format!("{service} ({account})"),
                PlaceKind::CloudStream,
                note,
            )
        }
        "mtp" | "gphoto2" => (host.replace('_', " "), PlaceKind::Device, None),
        _ => return None,
    };
    Some(Place {
        label,
        path,
        kind,
        unmount: Some(Unmount::Gio),
        note,
    })
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn percent_unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// kio-fuse exposes each share a KDE app opened as
/// `<root>/<protocol>/<host>/…`; list those as places.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
pub fn kio_fuse_places(root: &Path) -> Vec<Place> {
    let mut out = Vec::new();
    let Ok(protocols) = std::fs::read_dir(root) else {
        return out;
    };
    for protocol in protocols.flatten() {
        let scheme = protocol.file_name().to_string_lossy().to_string();
        if ![
            "smb", "sftp", "fish", "ftp", "ftps", "webdav", "webdavs", "nfs", "afp", "mtp",
        ]
        .contains(&scheme.as_str())
        {
            continue;
        }
        let Ok(hosts) = std::fs::read_dir(protocol.path()) else {
            continue;
        };
        for host in hosts.flatten() {
            let name = host.file_name().to_string_lossy().to_string();
            let kind = if scheme == "mtp" {
                PlaceKind::Device
            } else {
                PlaceKind::Network
            };
            out.push(Place {
                label: format!("{name} ({scheme})"),
                path: host.path(),
                kind,
                unmount: None,
                note: Some("Opened in a KDE app; shown here through kio-fuse.".into()),
            });
        }
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
/// Network and cloud mounts from `/proc/self/mounts` (kernel SMB/NFS, and
/// FUSE mounts such as sshfs, rclone and onedriver).
pub fn places_from_mounts(mounts: &str) -> Vec<Place> {
    let mut out = Vec::new();
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let (Some(device), Some(mount_point), Some(fs_type)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = PathBuf::from(unescape_mount(mount_point));
        let last = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let place = match fs_type {
            "cifs" | "smb3" => {
                // //server/share
                let mut it = device.trim_start_matches('/').splitn(2, '/');
                let (server, share) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
                Place {
                    label: format!("{share} on {server}"),
                    path,
                    kind: PlaceKind::Network,
                    unmount: None,
                    note: None,
                }
            }
            "nfs" | "nfs4" => Place {
                label: format!("{last} on {}", device.split(':').next().unwrap_or(device)),
                path,
                kind: PlaceKind::Network,
                unmount: None,
                note: None,
            },
            "fuse.sshfs" => Place {
                label: device.to_string(),
                path,
                kind: PlaceKind::Network,
                unmount: Some(Unmount::Fuse),
                note: None,
            },
            "davfs" | "fuse.davfs2" => Place {
                label: last,
                path,
                kind: PlaceKind::Network,
                unmount: None,
                note: None,
            },
            "fuse.rclone" => Place {
                label: format!("{} ({})", last, device.trim_end_matches(':')),
                path,
                kind: PlaceKind::CloudStream,
                unmount: Some(Unmount::Fuse),
                note: None,
            },
            "fuse.onedriver" => Place {
                label: format!("OneDrive ({last})"),
                path,
                kind: PlaceKind::CloudStream,
                unmount: Some(Unmount::Fuse),
                note: None,
            },
            _ => continue,
        };
        if !out.iter().any(|p: &Place| p.path == place.path) {
            out.push(place);
        }
    }
    out
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn unescape_mount(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 4 <= bytes.len()
            && bytes[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            out.push(u8::from_str_radix(&s[i + 1..i + 4], 8).unwrap_or(b'?'));
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Is `path` inside a cloud folder that streams files on demand? Thumbnails
/// there would download every file, so they're off unless the user opts in.
pub fn is_streaming_cloud(path: &Path, places: &[Place]) -> bool {
    places
        .iter()
        .any(|p| p.kind == PlaceKind::CloudStream && path.starts_with(&p.path))
}

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

/// If `text` is a network address rather than a local path, return it as a
/// URI: `smb://nas/photos`, `sftp://host/dir`, or a UNC path like
/// `\\nas\photos` (converted to `smb://`).
pub fn as_network_uri(text: &str) -> Option<String> {
    let text = text.trim();
    if let Some(unc) = text
        .strip_prefix("\\\\")
        .or_else(|| text.strip_prefix("//").filter(|_| cfg!(not(unix))))
    {
        let unc = unc.replace('\\', "/");
        return (!unc.is_empty()).then(|| format!("smb://{unc}"));
    }
    let (scheme, rest) = text.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let supported = [
        "smb", "sftp", "ssh", "ftp", "ftps", "dav", "davs", "webdav", "webdavs", "afp", "nfs",
    ];
    if !supported.contains(&scheme.as_str()) || rest.is_empty() {
        return None;
    }
    let scheme = match scheme.as_str() {
        "ssh" => "sftp",
        "webdav" => "dav",
        "webdavs" => "davs",
        s => s,
    };
    Some(format!("{scheme}://{rest}"))
}

/// The address without any password, safe to remember in settings.
pub fn redact_password(uri: &str) -> String {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return uri.to_string();
    };
    let (authority, tail) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => {
            let user = userinfo.split(':').next().unwrap_or("");
            format!("{scheme}://{user}@{host}{tail}")
        }
        None => uri.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Connecting
// ---------------------------------------------------------------------------

/// What the user typed into the sign-in dialog. Never stored.
/// Only the Linux connector signs in itself; Windows and macOS hand it to the OS.
#[derive(Clone, Default)]
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub struct Credentials {
    pub user: String,
    pub domain: String,
    pub password: String,
}

#[derive(Debug, PartialEq)]
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub enum ConnectError {
    /// The server wants a user name and password.
    NeedsCredentials {
        asks_domain: bool,
    },
    Failed(String),
}

/// Connect to a network address and return the local folder it's reachable at.
pub fn connect(uri: &str, creds: Option<&Credentials>) -> Result<PathBuf, ConnectError> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        #[cfg(test)]
        if let Some(gio) = TEST_GIO.lock().unwrap().clone() {
            return connect_with_gio(&gio, uri, creds);
        }
        connect_with_gio("gio", uri, creds)
    }
    #[cfg(windows)]
    {
        let _ = creds;
        // Windows reaches shares through UNC paths; it uses its own saved
        // credentials (map the drive once in Windows if a password is needed).
        let unc = uri
            .strip_prefix("smb://")
            .map(|rest| format!("\\\\{}", rest.replace('/', "\\")))
            .ok_or_else(|| {
                ConnectError::Failed(
                    "Only smb:// and \\\\server\\share addresses are supported on Windows".into(),
                )
            })?;
        let path = PathBuf::from(unc);
        if path.is_dir() {
            Ok(path)
        } else {
            Err(ConnectError::Failed("Can't reach that share. If it needs a password, map it as a network drive in Windows first.".into()))
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = creds;
        // Finder handles the sign-in and mounts it under /Volumes.
        let _ = std::process::Command::new("open").arg(uri).status();
        Err(ConnectError::Failed(
            "Opened in Finder. Once connected, the share appears under Network & Cloud.".into(),
        ))
    }
}

/// Lets UI tests substitute a scripted `gio`.
#[cfg(all(test, unix, not(target_os = "macos")))]
pub static TEST_GIO: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Drive `gio mount`, answering its prompts. Prompts are only answered from
/// `creds`; if the server asks for something we don't have, we stop and ask
/// the user rather than guessing.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn connect_with_gio(
    gio: &str,
    uri: &str,
    creds: Option<&Credentials>,
) -> Result<PathBuf, ConnectError> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let mut child = retry_busy(|| {
        Command::new(gio)
            .args(["mount", uri])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })
    .map_err(|e| {
            ConnectError::Failed(if e.kind() == std::io::ErrorKind::NotFound {
                "Connecting to servers uses GVfs, which isn't installed. Install the gvfs-backends \
                 package (on KDE you can instead open the share in Dolphin with kio-fuse installed; \
                 it then appears under Network & Cloud)."
                    .to_string()
            } else {
                format!("Couldn't run gio: {e}")
            })
        })?;

    // gio prints prompts without a newline, so read output as it arrives.
    let output = Arc::new(Mutex::new(String::new()));
    let mut readers = Vec::new();
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let output = output.clone();
        readers.push(std::thread::spawn(move || {
            let mut stream = stream;
            let mut buf = [0u8; 512];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                output
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }));
    }

    let mut stdin = child.stdin.take();
    let mut answered = 0; // prompts answered so far (by position in output)
    let mut asked_domain = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| ConnectError::Failed(e.to_string()))?
        {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err(ConnectError::Failed(
                "The server didn't respond in time".into(),
            ));
        }
        let text = output.lock().unwrap().clone();
        let pending = &text[answered.min(text.len())..];
        let prompt = [
            "User [",
            "User:",
            "Domain [",
            "Domain:",
            "Password:",
            "Choice:",
        ]
        .iter()
        .filter_map(|p| pending.find(p).map(|i| (i, *p)))
        .min_by_key(|(i, _)| *i);
        if let Some((at, prompt)) = prompt {
            // Wait until the prompt line is complete (ends with ": ").
            let after = &pending[at..];
            if after.contains(": ") || after.trim_end().ends_with(':') {
                let reply = match prompt {
                    "User [" | "User:" => creds.map(|c| c.user.clone()),
                    "Domain [" | "Domain:" => {
                        asked_domain = true;
                        creds.map(|c| c.domain.clone())
                    }
                    "Password:" => creds.map(|c| c.password.clone()),
                    _ => None, // a question we can't answer for the user (e.g. host key)
                };
                match (reply, stdin.as_mut()) {
                    (Some(reply), Some(input)) => {
                        // An empty user/domain accepts gio's default in brackets.
                        let _ = writeln!(input, "{reply}");
                        let _ = input.flush();
                        answered += at + after.find(':').map_or(after.len(), |i| i + 1);
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        if prompt == "Choice:" {
                            let question = pending
                                .lines()
                                .next()
                                .unwrap_or("The server asked a question")
                                .to_string();
                            return Err(ConnectError::Failed(format!(
                                "{question}. Connect once from a terminal (gio mount {uri}) to answer it."
                            )));
                        }
                        let asks_domain = asked_domain || pending.contains("Domain");
                        return Err(ConnectError::NeedsCredentials { asks_domain });
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    for r in readers {
        let _ = r.join();
    }
    let text = output.lock().unwrap().clone();
    if !status.success() && !text.contains("already mounted") {
        let message = text
            .lines()
            .rev()
            .find(|l| {
                l.trim_start().starts_with("gio:") || l.contains("Failed") || l.contains("rror")
            })
            .unwrap_or("Couldn't connect")
            .trim()
            .trim_start_matches("gio: ")
            .to_string();
        // "gio: smb://nas/x/: Failed to mount Windows share: Permission denied" -> the useful tail.
        let message = match message.split_once(": ") {
            Some((loc, rest)) if loc.contains("://") => rest.to_string(),
            _ => message,
        };
        if creds.is_some()
            && (message.contains("denied")
                || message.contains("ermission")
                || message.contains("ogon"))
        {
            return Err(ConnectError::Failed("Wrong user name or password".into()));
        }
        return Err(ConnectError::Failed(message));
    }
    local_path_of(gio, uri).ok_or_else(|| {
        ConnectError::Failed("Connected, but couldn't find the local folder for it".into())
    })
}

/// Starting a program can fail with "Text file busy" for a moment if the
/// file was just written (e.g. a package update replacing `gio`) while
/// another thread was forking; that clears up almost at once, so retry.
#[cfg(all(unix, not(target_os = "macos")))]
fn retry_busy<T>(mut run: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    for _ in 0..20 {
        match run() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(std::time::Duration::from_millis(25))
            }
            other => return other,
        }
    }
    run()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn local_path_of(gio: &str, uri: &str) -> Option<PathBuf> {
    let out = retry_busy(|| std::process::Command::new(gio).args(["info", uri]).output()).ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("local path: "))
        .map(|p| PathBuf::from(p.trim()))
}

/// Disconnect a place. Returns an error message for the user on failure.
pub fn disconnect(place: &Place) -> Result<(), String> {
    use std::process::Command;
    let ok = match place.unmount {
        Some(Unmount::Gio) => Command::new("gio")
            .args(["mount", "-u"])
            .arg(&place.path)
            .status()
            .is_ok_and(|s| s.success()),
        Some(Unmount::Fuse) => ["fusermount3", "fusermount"].iter().any(|prog| {
            Command::new(prog)
                .arg("-u")
                .arg(&place.path)
                .status()
                .is_ok_and(|s| s.success())
        }),
        None => {
            return Err(
                "This share was connected by the system and can't be disconnected here".into(),
            )
        }
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "Couldn't disconnect {}. Close anything using it and try again.",
            place.label
        ))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn gvfs_names_become_readable_labels() {
        let p = |n: &str| place_from_gvfs_name(n, PathBuf::from("/x")).unwrap();
        assert_eq!(
            p("smb-share:server=nas.local,share=photos").label,
            "photos on nas.local"
        );
        assert_eq!(
            p("smb-share:domain=WORK,server=nas,share=my%20files,user=tom").label,
            "my files on nas"
        );
        assert_eq!(p("sftp:host=example.com,user=tom").label, "tom@example.com");
        assert_eq!(
            p("mtp:host=Google_Pixel_7_1234").label,
            "Google Pixel 7 1234"
        );
        assert_eq!(p("mtp:host=Google_Pixel_7_1234").kind, PlaceKind::Device);
        let g = p("google-drive:host=gmail.com,user=tom");
        assert_eq!(g.label, "Google Drive (tom@gmail.com)");
        assert_eq!(g.kind, PlaceKind::CloudStream);
        assert!(g.note.is_some());
        assert!(place_from_gvfs_name("unknown-thing:x=y", PathBuf::new()).is_none());
    }

    #[test]
    fn mounts_become_places() {
        let mounts = "\
//nas/photos /mnt/photos cifs rw 0 0
server:/export/home /mnt/home nfs4 rw 0 0
gdrive: /home/tom/Cloud/GoogleDrive fuse.rclone rw 0 0
tom@host:/srv /home/tom/srv fuse.sshfs rw 0 0
/dev/sda1 / ext4 rw 0 0
";
        let places = places_from_mounts(mounts);
        let labels: Vec<&str> = places.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "photos on nas",
                "home on server",
                "GoogleDrive (gdrive)",
                "tom@host:/srv"
            ]
        );
        assert_eq!(places[2].kind, PlaceKind::CloudStream);
        assert!(is_streaming_cloud(
            Path::new("/home/tom/Cloud/GoogleDrive/Photos"),
            &places
        ));
        assert!(!is_streaming_cloud(Path::new("/mnt/photos/x"), &places));
    }

    #[test]
    fn kio_fuse_shares_become_places() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("smb/nas/photos")).unwrap();
        std::fs::create_dir_all(dir.path().join("sftp/tom@server")).unwrap();
        std::fs::create_dir_all(dir.path().join("trash")).unwrap();
        let places = kio_fuse_places(dir.path());
        let labels: Vec<&str> = places.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["nas (smb)", "tom@server (sftp)"]);
        assert_eq!(places[0].path, dir.path().join("smb/nas"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn missing_gio_gets_a_helpful_message() {
        match connect_with_gio("/nonexistent/gio", "smb://nas/x", None) {
            Err(ConnectError::Failed(m)) => assert!(m.contains("gvfs-backends"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn addresses_are_recognised() {
        assert_eq!(
            as_network_uri("\\\\nas\\photos\\2024").as_deref(),
            Some("smb://nas/photos/2024")
        );
        assert_eq!(
            as_network_uri("SMB://nas/photos").as_deref(),
            Some("smb://nas/photos")
        );
        assert_eq!(
            as_network_uri("ssh://me@host/home").as_deref(),
            Some("sftp://me@host/home")
        );
        assert_eq!(as_network_uri("/home/tom"), None);
        assert_eq!(as_network_uri("https://example.com"), None);
        assert_eq!(as_network_uri("smb://"), None);
        assert_eq!(
            redact_password("smb://tom:hunter2@nas/photos"),
            "smb://tom@nas/photos"
        );
        assert_eq!(redact_password("sftp://host/dir"), "sftp://host/dir");
    }

    /// A stand-in for `gio` that asks like the real one does for an SMB share.
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn fake_gio(dir: &Path) -> PathBuf {
        fake_gio_at(
            dir,
            Path::new("/run/user/1000/gvfs/smb-share:server=nas,share=photos"),
        )
    }

    /// `gio info` reports `local` as where the share is mounted.
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn fake_gio_at(dir: &Path, local: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("gio");
        std::fs::write(
            &script,
            r#"#!/bin/sh
if [ "$1" = "info" ]; then echo "uri: $2"; echo "local path: LOCAL_PATH"; exit 0; fi
if [ "$2" = "smb://nas/public" ]; then exit 0; fi
printf 'Password required for share photos on nas\nUser [tom]: '
read user
printf 'Domain [WORKGROUP]: '
read domain
printf 'Password: '
read pass
if [ "$user" = "tom" ] && [ "$pass" = "secret" ]; then exit 0; fi
echo "gio: smb://nas/photos/: Failed to mount Windows share: Permission denied" >&2
exit 2
"#
            .replace("LOCAL_PATH", &local.to_string_lossy()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn gio_connect_asks_only_when_needed_and_handles_wrong_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let gio = fake_gio(dir.path());
        let gio = gio.to_str().unwrap();
        let expected = PathBuf::from("/run/user/1000/gvfs/smb-share:server=nas,share=photos");

        // Anonymous share: no questions.
        assert_eq!(
            connect_with_gio(gio, "smb://nas/public", None),
            Ok(expected.clone())
        );
        // Protected share without credentials: ask the user.
        assert_eq!(
            connect_with_gio(gio, "smb://nas/photos", None),
            Err(ConnectError::NeedsCredentials { asks_domain: false })
        );
        let creds = |pass: &str| Credentials {
            user: "tom".into(),
            domain: String::new(),
            password: pass.into(),
        };
        assert_eq!(
            connect_with_gio(gio, "smb://nas/photos", Some(&creds("nope"))),
            Err(ConnectError::Failed("Wrong user name or password".into()))
        );
        assert_eq!(
            connect_with_gio(gio, "smb://nas/photos", Some(&creds("secret"))),
            Ok(expected)
        );
    }
}
