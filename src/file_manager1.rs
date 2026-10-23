// Ninimma modification: independent project branding and integration.
//! `org.freedesktop.FileManager1` on the session bus: what browsers, mail
//! and download managers call for "Show in folder". While this app runs it
//! answers, unless another file manager already does (that one keeps it).

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;

/// What another app asked to show.
#[derive(Debug, PartialEq)]
pub enum Request {
    /// Open these folders.
    Folders(Vec<PathBuf>),
    /// Open the folder holding these and select them.
    Items(Vec<PathBuf>),
}

#[cfg_attr(test, allow(dead_code))] // tests never claim the real bus name
struct Service {
    tx: Sender<Request>,
    ctx: egui::Context,
}

#[cfg_attr(test, allow(dead_code))]
impl Service {
    fn send(&self, request: Request) {
        if self.tx.send(request).is_ok() {
            self.ctx.request_repaint();
        }
    }
}

#[cfg_attr(test, allow(dead_code))]
#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl Service {
    fn show_folders(&self, uris: Vec<String>, _startup_id: &str) {
        self.send(Request::Folders(paths(&uris)));
    }

    fn show_items(&self, uris: Vec<String>, _startup_id: &str) {
        self.send(Request::Items(paths(&uris)));
    }

    /// No properties window from here: shows the items instead.
    fn show_item_properties(&self, uris: Vec<String>, _startup_id: &str) {
        self.send(Request::Items(paths(&uris)));
    }
}

/// Local paths from `file://` URIs; anything else is ignored.
pub fn paths(uris: &[String]) -> Vec<PathBuf> {
    uris.iter()
        .filter_map(|u| {
            let rest = u.strip_prefix("file://")?;
            // file://host/path: only this computer's files.
            let path = rest.strip_prefix("localhost").unwrap_or(rest);
            path.starts_with('/')
                .then(|| PathBuf::from(crate::ui::percent_decode(path)))
        })
        .collect()
}

#[cfg_attr(test, allow(dead_code))]
/// Starts answering in the background. The receiver gets the requests.
pub fn start(ctx: &egui::Context) -> Receiver<Request> {
    static CONNECTION: OnceLock<zbus::blocking::Connection> = OnceLock::new();
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("file-manager1".into())
        .spawn(move || {
            let built = zbus::blocking::connection::Builder::session()
                .and_then(|b| b.serve_at("/org/freedesktop/FileManager1", Service { tx, ctx }))
                .and_then(|b| b.name("org.freedesktop.FileManager1"))
                // Hand it back if the desktop's own file manager asks.
                .map(|b| b.allow_name_replacements(true))
                .and_then(|b| b.build());
            // Kept for the life of the app; without a bus, or with another
            // file manager answering, nothing to do.
            if let Ok(connection) = built {
                let _ = CONNECTION.set(connection);
            }
        })
        .expect("thread spawns");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_file_uris_become_paths() {
        let uris = [
            "file:///home/me/Downloads/My%20Report.pdf".to_string(),
            "file://localhost/tmp/a".to_string(),
            "file://nas/share/b".to_string(),
            "https://example.com/c".to_string(),
        ];
        assert_eq!(
            paths(&uris),
            [
                PathBuf::from("/home/me/Downloads/My Report.pdf"),
                PathBuf::from("/tmp/a")
            ]
        );
    }
}
