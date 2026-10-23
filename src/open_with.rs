// Ninimma modification: independent project branding and integration.
//! "Open with…" on Linux and FreeBSD: the applications that can open a
//! file, its default, launching one, and making one the default.
//!
//! Follows the freedesktop specifications every desktop uses: apps are the
//! `.desktop` files in `applications/` under the XDG data folders
//! (Flatpak's exports included, as they're on XDG_DATA_DIRS); file types
//! come from shared-mime-info's `globs2`; defaults and associations from
//! `mimeapps.list`. An app is started directly from its `Exec=` line,
//! never through a shell, so nothing in a file name can run as a command.
//! Setting a default uses `xdg-mime`, so the whole desktop agrees.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// An application that can open files.
#[derive(Clone, Debug, PartialEq)]
pub struct App {
    /// Desktop file id, e.g. "org.gnome.eog.desktop".
    pub id: String,
    pub name: String,
    pub exec: String,
    pub mime_types: Vec<String>,
}

/// The installed applications, the type of each extension, and the user's
/// associations, read once.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub apps: Vec<App>,
    /// Lower-case extension → MIME type.
    globs: HashMap<String, String>,
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, HashSet<String>>,
}

/// Folders, in priority order (user first).
pub struct Dirs {
    /// Data folders: each has `applications/` and `mime/`.
    pub data: Vec<PathBuf>,
    /// Folders holding `mimeapps.list`.
    pub config: Vec<PathBuf>,
    /// The session's desktops, lower-case ("zorin", "gnome"): each folder's
    /// `<desktop>-mimeapps.list` comes before its `mimeapps.list`.
    pub desktops: Vec<String>,
}

impl Dirs {
    /// The XDG folders of this session.
    pub fn from_env() -> Self {
        let env_dir = |n: &str| {
            std::env::var_os(n)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        let env_list = |n: &str, default: &str| -> Vec<PathBuf> {
            std::env::var(n)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| default.to_string())
                .split(':')
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .collect()
        };
        let home = crate::file_system::home_dir();
        let mut data: Vec<PathBuf> = env_dir("XDG_DATA_HOME")
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")))
            .into_iter()
            .collect();
        data.extend(env_list("XDG_DATA_DIRS", "/usr/local/share:/usr/share"));
        let mut config: Vec<PathBuf> = env_dir("XDG_CONFIG_HOME")
            .or_else(|| home.as_ref().map(|h| h.join(".config")))
            .into_iter()
            .collect();
        config.extend(env_list("XDG_CONFIG_DIRS", "/etc/xdg"));
        // mimeapps.list may also sit in each data folder's applications/.
        config.extend(data.iter().map(|d| d.join("applications")));
        let desktops = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .split(':')
            .filter(|d| !d.is_empty())
            .map(str::to_lowercase)
            .collect();
        Self {
            data,
            config,
            desktops,
        }
    }
}

impl Catalog {
    pub fn load(dirs: &Dirs) -> Self {
        let mut catalog = Catalog::default();
        // Apps: the first folder with a given id wins (user overrides system).
        let mut seen = HashSet::new();
        for data in &dirs.data {
            let apps_dir = data.join("applications");
            for (id, path) in desktop_files(&apps_dir) {
                if !seen.insert(id.clone()) {
                    continue;
                }
                if let Some(app) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| parse_desktop_entry(&id, &text))
                {
                    catalog.apps.push(app);
                }
            }
        }
        catalog.apps.sort_by_key(|a| a.name.to_lowercase());
        // Types: the system's glob list (later folders don't override).
        for data in dirs.data.iter().rev() {
            if let Ok(text) = std::fs::read_to_string(data.join("mime/globs2")) {
                for line in text.lines().filter(|l| !l.starts_with('#')) {
                    // weight:mime/type:*.ext[:flags]
                    let mut parts = line.split(':');
                    let (Some(_weight), Some(mime), Some(glob)) =
                        (parts.next(), parts.next(), parts.next())
                    else {
                        continue;
                    };
                    if let Some(ext) = glob
                        .strip_prefix("*.")
                        .filter(|e| !e.contains(['*', '?', '[']))
                    {
                        catalog
                            .globs
                            .entry(ext.to_lowercase())
                            .or_insert_with(|| mime.to_string());
                    }
                }
            }
        }
        // Associations: earlier files take precedence.
        // (Desktops keep their defaults in e.g. gnome-mimeapps.list.)
        for config in &dirs.config {
            let names = dirs
                .desktops
                .iter()
                .map(|d| format!("{d}-mimeapps.list"))
                .chain(["mimeapps.list".to_string()]);
            for name in names {
                if let Ok(text) = std::fs::read_to_string(config.join(name)) {
                    catalog.read_mimeapps(&text);
                }
            }
        }
        catalog
    }

    fn read_mimeapps(&mut self, text: &str) {
        let mut section = "";
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line;
                continue;
            }
            let Some((mime, ids)) = line.split_once('=') else {
                continue;
            };
            let ids: Vec<String> = ids
                .split(';')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            match section {
                "[Default Applications]" => {
                    self.defaults.entry(mime.to_string()).or_insert(ids);
                }
                "[Added Associations]" => {
                    self.added.entry(mime.to_string()).or_default().extend(ids)
                }
                "[Removed Associations]" => self
                    .removed
                    .entry(mime.to_string())
                    .or_default()
                    .extend(ids),
                _ => {}
            }
        }
    }

    /// The MIME type of a file, by its extension ("inode/directory" for
    /// folders).
    pub fn mime_of(&self, path: &Path) -> Option<String> {
        if path.is_dir() {
            return Some("inode/directory".into());
        }
        let ext = path.extension()?.to_string_lossy().to_lowercase();
        self.globs.get(&ext).cloned()
    }

    /// The default app for a type, then the others that can open it.
    pub fn apps_for(&self, mime: &str) -> (Option<&App>, Vec<&App>) {
        let by_id = |id: &str| self.apps.iter().find(|a| a.id == id);
        let removed = self.removed.get(mime);
        let allowed = |a: &&App| removed.is_none_or(|r| !r.contains(&a.id));
        let default = self
            .defaults
            .get(mime)
            .and_then(|ids| ids.iter().find_map(|id| by_id(id)))
            .filter(allowed);
        let mut others: Vec<&App> = self
            .added
            .get(mime)
            .into_iter()
            .flatten()
            .filter_map(|id| by_id(id))
            .chain(
                self.apps
                    .iter()
                    .filter(|a| a.mime_types.iter().any(|m| m == mime)),
            )
            .filter(allowed)
            .filter(|a| Some(a.id.as_str()) != default.map(|d| d.id.as_str()))
            .collect();
        let mut seen = HashSet::new();
        others.retain(|a| seen.insert(a.id.clone()));
        (default, others)
    }
}

/// `.desktop` files under `dir`, with their ids ("sub/app.desktop" is
/// "sub-app.desktop").
fn desktop_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let path = e.path();
            if path.is_dir() {
                stack.push((path, format!("{prefix}{name}-")));
            } else if name.ends_with(".desktop") {
                out.push((format!("{prefix}{name}"), path));
            }
        }
    }
    out.sort();
    out
}

/// An application from a desktop entry, if it's one to offer: visible,
/// not run in a terminal, with something to run.
pub fn parse_desktop_entry(id: &str, text: &str) -> Option<App> {
    let mut in_entry = false;
    let (mut name, mut exec, mut mimes) = (None, None, Vec::new());
    let mut kind = None;
    let mut hidden = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Name" => name = Some(value.trim().to_string()),
            "Exec" => exec = Some(value.trim().to_string()),
            "MimeType" => {
                mimes = value
                    .split(';')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            }
            "Type" => kind = Some(value.trim().to_string()),
            "NoDisplay" | "Hidden" if value.trim() == "true" => hidden = true,
            // Terminal apps aren't offered yet (they need a terminal to run in).
            "Terminal" if value.trim() == "true" => hidden = true,
            _ => {}
        }
    }
    if hidden || kind.as_deref().is_some_and(|k| k != "Application") {
        return None;
    }
    Some(App {
        id: id.to_string(),
        name: name?,
        exec: exec?,
        mime_types: mimes,
    })
}

/// The program and arguments to open `files` with an app, from its Exec
/// line: quoted words are kept together, %f/%F/%u/%U become the files
/// (paths, or file:// URLs for %u/%U), %c the app's name, "%%" a percent;
/// other field codes are dropped. Without a file code, the files go last.
pub fn command_line(app: &App, files: &[PathBuf]) -> Option<(String, Vec<String>)> {
    let mut words: Vec<String> = Vec::new();
    let mut used_files = false;
    for word in split_exec(&app.exec)? {
        match word.as_str() {
            "%f" | "%F" => {
                used_files = true;
                let take = if word == "%f" { 1 } else { files.len() };
                words.extend(
                    files
                        .iter()
                        .take(take)
                        .map(|p| p.to_string_lossy().to_string()),
                );
            }
            "%u" | "%U" => {
                used_files = true;
                let take = if word == "%u" { 1 } else { files.len() };
                words.extend(files.iter().take(take).map(|p| file_url(p)));
            }
            _ => {
                let mut out = String::new();
                let mut chars = word.chars().peekable();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        out.push(c);
                        continue;
                    }
                    match chars.next() {
                        Some('%') => out.push('%'),
                        Some('c') => out.push_str(&app.name),
                        _ => {} // %i %k and deprecated codes: dropped
                    }
                }
                if !out.is_empty() {
                    words.push(out);
                }
            }
        }
    }
    if !used_files {
        words.extend(files.iter().map(|p| p.to_string_lossy().to_string()));
    }
    let mut words = words.into_iter();
    Some((words.next()?, words.collect()))
}

/// Splits an Exec value into words: double quotes group, and inside them
/// a backslash escapes `"`, `` ` ``, `$` and `\`.
fn split_exec(exec: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut in_word = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            '\\' if quoted => word.push(chars.next()?),
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quoted {
        return None; // unbalanced quotes: not run
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

/// A local path as a file:// URL, with everything but unreserved
/// characters and '/' escaped.
fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = path.to_string_lossy().as_bytes().to_vec();
    for b in bytes {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Starts an app on `files`, detached. (Tests record instead.)
#[cfg_attr(test, allow(dead_code))]
pub fn launch(app: &App, files: &[PathBuf]) -> Result<(), String> {
    let (program, args) =
        command_line(app, files).ok_or_else(|| format!("{} can't be started", app.name))?;
    std::process::Command::new(&program)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't start {}: {e}", app.name))
}

/// Makes `app` the default for `mime`, for the whole desktop. (Tests
/// record instead: they never change the real associations.)
#[cfg_attr(test, allow(dead_code))]
pub fn set_default(app: &App, mime: &str) -> Result<(), String> {
    let status = std::process::Command::new("xdg-mime")
        .args(["default", &app.id, mime])
        .status()
        .map_err(|_| "xdg-mime isn't installed (xdg-utils)".to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "xdg-mime couldn't set the default".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn apps_defaults_and_types_come_from_the_xdg_folders() {
        let root = tempfile::tempdir().unwrap();
        let (user, system, config) = (
            root.path().join("user"),
            root.path().join("sys"),
            root.path().join("cfg"),
        );
        write(
            &system.join("mime/globs2"),
            "50:image/jpeg:*.jpg\n50:image/jpeg:*.JPEG\n50:application/pdf:*.pdf\n",
        );
        write(&system.join("applications/viewer.desktop"), "[Desktop Entry]\nType=Application\nName=Image Viewer\nExec=viewer %F\nMimeType=image/jpeg;image/png;\n");
        write(
            &system.join("applications/gimp.desktop"),
            "[Desktop Entry]\nType=Application\nName=GIMP\nExec=gimp %U\nMimeType=image/jpeg;\n",
        );
        write(
            &system.join("applications/secret.desktop"),
            "[Desktop Entry]\nName=Hidden\nExec=x\nNoDisplay=true\nMimeType=image/jpeg;\n",
        );
        write(
            &system.join("applications/vim.desktop"),
            "[Desktop Entry]\nName=Vim\nExec=vim %F\nTerminal=true\nMimeType=text/plain;\n",
        );
        // The user's copy of an app overrides the system's.
        write(
            &user.join("applications/gimp.desktop"),
            "[Desktop Entry]\nName=GIMP (mine)\nExec=gimp-2 %U\nMimeType=image/jpeg;\n",
        );
        write(
            &system.join("applications/kde/okular.desktop"),
            "[Desktop Entry]\nName=Okular\nExec=okular %U\nMimeType=application/pdf;\n",
        );
        write(&config.join("mimeapps.list"), "[Default Applications]\nimage/jpeg=gimp.desktop;\n[Removed Associations]\nimage/png=viewer.desktop;\n");
        // The desktop's own defaults, used where the user's file says nothing.
        write(
            &system.join("applications/gnome-mimeapps.list"),
            "[Default Applications]\napplication/pdf=kde-okular.desktop;\nimage/jpeg=viewer.desktop;\n",
        );
        let catalog = Catalog::load(&Dirs {
            data: vec![user, system.clone()],
            config: vec![config, system.join("applications")],
            desktops: vec!["gnome".into()],
        });

        assert_eq!(
            catalog.mime_of(Path::new("/x/Photo.JPG")).as_deref(),
            Some("image/jpeg")
        );
        let (default, others) = catalog.apps_for("image/jpeg");
        assert_eq!(default.unwrap().name, "GIMP (mine)");
        assert_eq!(
            others.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Image Viewer"]
        );
        assert!(
            catalog
                .apps
                .iter()
                .all(|a| a.name != "Hidden" && a.name != "Vim"),
            "hidden and terminal apps aren't offered"
        );
        let (default, others) = catalog.apps_for("image/png");
        assert!(
            default.is_none() && others.is_empty(),
            "removed association"
        );
        // Apps in subfolders get "kde-okular.desktop" as their id.
        let (pdf, _) = catalog.apps_for("application/pdf");
        assert_eq!(
            pdf.unwrap().id,
            "kde-okular.desktop",
            "from gnome-mimeapps.list"
        );
    }

    #[test]
    fn exec_lines_become_arguments_without_a_shell() {
        let app = |exec: &str| App {
            id: "a.desktop".into(),
            name: "My App".into(),
            exec: exec.into(),
            mime_types: vec![],
        };
        let files = [
            PathBuf::from("/tmp/a b.txt"),
            PathBuf::from("/tmp/$(rm -rf ~).txt"),
        ];
        // %F: every file as its own argument, untouched by any shell.
        assert_eq!(
            command_line(&app("editor --new-window %F"), &files).unwrap(),
            (
                "editor".into(),
                vec![
                    "--new-window".into(),
                    "/tmp/a b.txt".into(),
                    "/tmp/$(rm -rf ~).txt".into()
                ]
            )
        );
        // %u: one file as a URL; %c the name; %% a percent; %i dropped.
        assert_eq!(
            command_line(&app(r#""/opt/My App/run" --title=%c %i 100%% %u"#), &files).unwrap(),
            (
                "/opt/My App/run".into(),
                vec![
                    "--title=My App".into(),
                    "100%".into(),
                    "file:///tmp/a%20b.txt".into()
                ]
            )
        );
        // No field code: files go last.
        assert_eq!(
            command_line(&app("viewer"), &files[..1]).unwrap().1,
            ["/tmp/a b.txt"]
        );
        // Unbalanced quotes aren't run at all.
        assert!(command_line(&app(r#"viewer "%F"#), &files).is_none());
    }
}

/// Prints what this desktop offers (reads the real desktop, so opt-in):
/// `cargo test -- --ignored --nocapture apps_on_this_desktop`.
#[cfg(test)]
#[test]
#[ignore]
fn apps_on_this_desktop() {
    let catalog = Catalog::load(&Dirs::from_env());
    eprintln!("{} apps", catalog.apps.len());
    for file in ["a.txt", "a.pdf", "a.jpg", "a.mp4", "a.ods", "a.docx"] {
        let mime = catalog.mime_of(Path::new(file));
        let (default, others) = mime
            .as_deref()
            .map(|m| catalog.apps_for(m))
            .unwrap_or_default();
        eprintln!(
            "{file}: {mime:?} default={:?} others={:?}",
            default.map(|a| &a.name),
            others.iter().map(|a| &a.name).take(6).collect::<Vec<_>>()
        );
    }
}
