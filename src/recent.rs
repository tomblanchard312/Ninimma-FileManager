// Ninimma modification: independent project branding and integration.
//! Recent files and folders, for the Recent view.
//!
//! Two sources, newest first: what was opened and visited in this app
//! (kept in the settings, capped), and on Linux and FreeBSD the desktop's
//! shared list (`recently-used.xbel`, written by GTK and KDE apps), so
//! Recent matches what Dolphin and Files show. Entries for files that no
//! longer exist are left out. Nothing leaves this computer.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Entries kept in the app's own list.
pub const MAX_OWN: usize = 50;
/// Entries shown in the Recent view.
pub const MAX_SHOWN: usize = 100;
/// The desktop's list is read only up to this size.
#[cfg_attr(windows, allow(dead_code))] // read from the desktop on Linux/FreeBSD only
const MAX_XBEL_BYTES: u64 = 4 * 1024 * 1024;

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Puts `path` first in `list` (once), keeping at most `MAX_OWN`.
pub fn record(list: &mut Vec<(PathBuf, i64)>, path: &Path, when: i64) {
    list.retain(|(p, _)| p != path);
    list.insert(0, (path.to_path_buf(), when));
    list.truncate(MAX_OWN);
}

/// The desktop's shared recent files, if this platform keeps one.
pub fn desktop_recent() -> Vec<(PathBuf, i64)> {
    // Tests never read the user's real list (the parser has its own test).
    if cfg!(test) {
        return Vec::new();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| crate::file_system::home_dir().map(|h| h.join(".local/share")));
        if let Some(file) = data.map(|d| d.join("recently-used.xbel")) {
            if std::fs::metadata(&file).is_ok_and(|m| m.len() <= MAX_XBEL_BYTES) {
                if let Ok(text) = std::fs::read_to_string(&file) {
                    return parse_xbel(&text);
                }
            }
        }
    }
    Vec::new()
}

/// Recent entries from both lists, newest first, each once, only those
/// that still exist, at most `MAX_SHOWN`.
pub fn merged(own: &[(PathBuf, i64)], desktop: &[(PathBuf, i64)]) -> Vec<PathBuf> {
    let mut all: Vec<&(PathBuf, i64)> = own.iter().chain(desktop).collect();
    all.sort_by_key(|(_, when)| std::cmp::Reverse(*when));
    let mut seen = HashSet::new();
    all.into_iter()
        .filter(|(p, _)| seen.insert(p.clone()))
        .filter(|(p, _)| p.exists())
        .map(|(p, _)| p.clone())
        .take(MAX_SHOWN)
        .collect()
}

/// Local files in an XBEL list, with when each was last used.
#[cfg_attr(windows, allow(dead_code))] // read from the desktop on Linux/FreeBSD only
fn parse_xbel(text: &str) -> Vec<(PathBuf, i64)> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(text);
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == "bookmark" => {
                let mut href = None;
                let mut when = 0;
                for a in e.attributes().flatten() {
                    let value = a.value.to_string();
                    match a.key.as_ref() {
                        "href" => href = Some(value),
                        // The latest of the three times.
                        "visited" | "modified" | "added" => {
                            when = when.max(parse_time(&value).unwrap_or(0))
                        }
                        _ => {}
                    }
                }
                if let Some(path) = href.as_deref().and_then(file_uri_to_path) {
                    out.push((path, when));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// "2026-10-12T10:00:00.123456Z" as seconds since the epoch.
#[cfg_attr(windows, allow(dead_code))] // read from the desktop on Linux/FreeBSD only
fn parse_time(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp())
}

/// A local `file://` URI as a path ("%20" and other escapes decoded).
#[cfg_attr(windows, allow(dead_code))] // read from the desktop on Linux/FreeBSD only
fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Only this machine's files ("file:///..." or "file://localhost/...").
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        let escaped = (raw[i] == b'%' && i + 2 < raw.len())
            .then(|| std::str::from_utf8(&raw[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(byte) = escaped {
            bytes.push(byte);
            i += 3;
        } else {
            // A "%" that isn't an escape is kept as it is.
            bytes.push(raw[i]);
            i += 1;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(bytes).ok().map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_own_list_keeps_newest_first_once_and_bounded() {
        let mut list = Vec::new();
        for i in 0..(MAX_OWN + 5) {
            record(&mut list, Path::new(&format!("/f{i}")), i as i64);
        }
        record(&mut list, Path::new("/f10"), 1000);
        assert_eq!(list.len(), MAX_OWN);
        assert_eq!(list[0], (PathBuf::from("/f10"), 1000));
        assert_eq!(
            list.iter().filter(|(p, _)| p == Path::new("/f10")).count(),
            1
        );
    }

    #[test]
    fn the_desktop_list_is_read_with_its_times_and_escapes() {
        let xbel = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0" xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks">
  <bookmark href="file:///home/tom/Tax%20Return%202024.pdf" added="2026-10-01T09:00:00Z" modified="2026-10-02T09:00:00Z" visited="2026-10-03T09:00:00.123456Z">
    <info/>
  </bookmark>
  <bookmark href="file:///home/tom/caf%C3%A9.txt" added="2026-09-01T09:00:00Z" modified="2026-09-01T09:00:00Z" visited="2026-09-01T09:00:00Z"/>
  <bookmark href="https://example.com/page" added="2026-10-05T09:00:00Z" modified="2026-10-05T09:00:00Z" visited="2026-10-05T09:00:00Z"/>
  <bookmark href="file://otherhost/share/x" added="2026-10-05T09:00:00Z"/>
</xbel>"#;
        let found = parse_xbel(xbel);
        assert_eq!(
            found.len(),
            2,
            "web pages and other hosts are skipped: {found:?}"
        );
        assert_eq!(found[0].0, PathBuf::from("/home/tom/Tax Return 2024.pdf"));
        assert_eq!(
            found[0].1,
            parse_time("2026-10-03T09:00:00Z").unwrap(),
            "the latest time"
        );
        assert_eq!(found[1].0, PathBuf::from("/home/tom/café.txt"));
        // Malformed input gives what could be read, no panic.
        assert!(parse_xbel("<xbel><bookmark href=").is_empty());
        assert_eq!(file_uri_to_path("file:///a%2"), Some(PathBuf::from("/a%2")));
        assert_eq!(
            file_uri_to_path("file:///a%zz"),
            Some(PathBuf::from("/a%zz"))
        );
    }

    #[test]
    fn merged_is_newest_first_once_and_only_existing() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "").unwrap();
        let gone = dir.path().join("gone.txt");
        let own = vec![(a.clone(), 10), (gone, 30)];
        let desktop = vec![(b.clone(), 20), (a.clone(), 5)];
        assert_eq!(merged(&own, &desktop), [b, a]);
    }
}
