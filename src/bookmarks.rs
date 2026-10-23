// Ninimma modification: independent project branding and integration.
//! Bookmarked folders. On Linux these live in GTK's bookmarks file, so they
//! are shared with Nemo, Nautilus and the file chooser; elsewhere they're
//! kept in this app's own settings.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub label: String,
    pub path: PathBuf,
}

/// Where GTK keeps bookmarks, if this platform uses it.
pub fn gtk_bookmarks_file() -> Option<PathBuf> {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return None;
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| crate::file_system::home_dir().map(|h| h.join(".config")))?;
    Some(config.join("gtk-3.0").join("bookmarks"))
}

/// KDE's places file (Dolphin's sidebar), if this platform uses it.
pub fn dolphin_places_file() -> Option<PathBuf> {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return None;
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| crate::file_system::home_dir().map(|h| h.join(".local/share")))?;
    Some(data.join("user-places.xbel"))
}

/// The user's own places from Dolphin's XBEL file: local folders only,
/// skipping KDE's built-in entries (Home, Trash, Network…) and hidden ones.
pub fn parse_dolphin_places(xml: &str) -> Vec<Bookmark> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = Vec::new();
    let mut current: Option<(String, Option<String>, bool)> = None; // (href, title, skip)
    let mut field = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_string();
                if name == "bookmark" {
                    let href = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == "href")
                        .map(|a| a.value.to_string())
                        .unwrap_or_default();
                    current = Some((href, None, false));
                }
                field = name;
            }
            Ok(Event::Text(t)) => {
                let text = t.trim().to_string();
                if let Some((_, title, skip)) = current.as_mut() {
                    match field.as_str() {
                        "title" if title.is_none() => *title = Some(text),
                        "isSystemItem" | "IsHidden" if text == "true" => *skip = true,
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == "bookmark" {
                    if let Some((href, title, skip)) = current.take() {
                        if let (false, Some(path)) = (skip, href.strip_prefix("file://")) {
                            let path = PathBuf::from(percent_decode(path));
                            let label = title
                                .filter(|t| !t.is_empty())
                                .unwrap_or_else(|| crate::file_ops::display_name(&path));
                            out.push(Bookmark { label, path });
                        }
                    }
                }
                field.clear();
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Parse GTK's format: one `file:///path optional label` per line. Remote
/// bookmarks (smb://, sftp://) are skipped: they're reached through
/// "Connect to server" instead.
pub fn parse(contents: &str) -> Vec<Bookmark> {
    contents
        .lines()
        .filter_map(|line| {
            let (uri, label) = match line.trim().split_once(' ') {
                Some((u, l)) => (u, Some(l.trim())),
                None => (line.trim(), None),
            };
            let path = PathBuf::from(percent_decode(uri.strip_prefix("file://")?));
            let label = label
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| crate::file_ops::display_name(&path));
            Some(Bookmark { label, path })
        })
        .collect()
}

pub fn serialize(bookmarks: &[Bookmark]) -> String {
    bookmarks
        .iter()
        .map(|b| {
            let uri = file_uri(&b.path);
            if b.label == crate::file_ops::display_name(&b.path) {
                format!("{uri}\n")
            } else {
                format!("{uri} {}\n", b.label)
            }
        })
        .collect()
}

/// Read bookmarks from `file`, keeping lines we don't understand (remote
/// bookmarks) untouched when writing back.
pub fn load(file: &Path) -> Vec<Bookmark> {
    std::fs::read_to_string(file)
        .map(|s| parse(&s))
        .unwrap_or_default()
}

/// Add or remove one bookmark in `file`, preserving every other line
/// (including other apps' remote bookmarks) exactly.
pub fn update(file: &Path, add: Option<&Bookmark>, remove: Option<&Path>) -> Result<(), String> {
    let existing = std::fs::read_to_string(file).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|line| {
            let keep_remove =
                remove.is_none_or(|r| parse(line).first().is_none_or(|b| b.path != r));
            let keep_dupe =
                add.is_none_or(|a| parse(line).first().is_none_or(|b| b.path != a.path));
            keep_remove && keep_dupe
        })
        .map(str::to_string)
        .collect();
    if let Some(a) = add {
        lines.push(serialize(std::slice::from_ref(a)).trim_end().to_string());
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't save bookmarks: {e}"))?;
    }
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    std::fs::write(file, text).map_err(|e| format!("Couldn't save bookmarks: {e}"))
}

fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.to_string_lossy().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_gtk_bookmarks_like_nemo_writes_them() {
        let text =
            "file:///data/archive Archive\nfile:///home/tom/My%20Photos\nsftp://host/dir Remote\n";
        let b = parse(text);
        assert_eq!(
            b,
            [
                Bookmark {
                    label: "Archive".into(),
                    path: "/data/archive".into()
                },
                Bookmark {
                    label: "My Photos".into(),
                    path: "/home/tom/My Photos".into()
                },
            ]
        );
        assert_eq!(
            serialize(&b),
            "file:///data/archive Archive\nfile:///home/tom/My%20Photos\n"
        );
    }

    #[test]
    fn reads_dolphin_places_skipping_system_and_hidden_ones() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE xbel>
<xbel xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks">
 <bookmark href="file:///home/tom">
  <title>Home</title>
  <info><metadata owner="http://www.kde.org"><ID>1/0</ID><isSystemItem>true</isSystemItem></metadata></info>
 </bookmark>
 <bookmark href="file:///data/My%20Projects">
  <title>Projects</title>
  <info><metadata owner="http://www.kde.org"><ID>1/5</ID><IsHidden>false</IsHidden></metadata></info>
 </bookmark>
 <bookmark href="file:///data/old">
  <title>Old</title>
  <info><metadata owner="http://www.kde.org"><IsHidden>true</IsHidden></metadata></info>
 </bookmark>
 <bookmark href="trash:/"><title>Trash</title></bookmark>
 <bookmark href="file:///mnt/nas"><title></title></bookmark>
</xbel>"#;
        let b = parse_dolphin_places(xml);
        assert_eq!(
            b,
            [
                Bookmark {
                    label: "Projects".into(),
                    path: "/data/My Projects".into()
                },
                Bookmark {
                    label: "nas".into(),
                    path: "/mnt/nas".into()
                },
            ]
        );
    }

    #[test]
    fn add_and_remove_keep_other_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("gtk-3.0/bookmarks");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "sftp://host/dir Remote\nfile:///data/archive Archive\n",
        )
        .unwrap();
        let new = Bookmark {
            label: "Pics".into(),
            path: "/home/tom/Pictures".into(),
        };
        update(&file, Some(&new), None).unwrap();
        update(&file, Some(&new), None).unwrap(); // no duplicates
        assert_eq!(load(&file).len(), 2);
        update(&file, None, Some(Path::new("/data/archive"))).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "sftp://host/dir Remote\nfile:///home/tom/Pictures Pics\n"
        );
    }
}
