// Ninimma modification: independent project branding and integration.
//! Files on the system clipboard, so other apps can paste what's copied
//! here and the other way round: a `text/uri-list` on Linux and FreeBSD,
//! CF_HDROP on Windows, a file list on macOS. Tests use a stand-in, so they
//! never touch the real clipboard.

use std::path::PathBuf;

#[cfg(test)]
use fake as imp;
#[cfg(not(test))]
use real as imp;

/// Puts `paths` on the clipboard as files. False if there's no clipboard.
pub fn set_files(paths: &[PathBuf]) -> bool {
    imp::set_files(paths)
}

/// Files another app (or this one) put on the clipboard.
pub fn files() -> Option<Vec<PathBuf>> {
    imp::files()
}

pub fn clear() {
    imp::clear()
}

/// What another file manager copied, as if the user had copied it here.
#[cfg(test)]
pub fn copied_elsewhere(paths: &[&std::path::Path]) {
    imp::set_files(paths);
}

#[cfg(not(test))]
mod real {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    // Kept for the life of the app: on X11 the clipboard's owner serves it
    // to other apps, and dropping the last handle hands it off.
    static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

    fn with<T>(f: impl FnOnce(&mut arboard::Clipboard) -> Option<T>) -> Option<T> {
        let mut guard = CLIPBOARD.lock().ok()?;
        if guard.is_none() {
            *guard = arboard::Clipboard::new().ok();
        }
        f(guard.as_mut()?)
    }

    pub fn set_files(paths: &[impl AsRef<Path>]) -> bool {
        with(|c| c.set().file_list(paths).ok()).is_some()
    }

    pub fn files() -> Option<Vec<PathBuf>> {
        with(|c| c.get().file_list().ok()).filter(|f| !f.is_empty())
    }

    pub fn clear() {
        with(|c| c.clear().ok());
    }
}

#[cfg(test)]
mod fake {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    thread_local! {
        pub static FAKE: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
    }

    pub fn set_files(paths: &[impl AsRef<Path>]) -> bool {
        FAKE.with(|f| {
            *f.borrow_mut() = Some(paths.iter().map(|p| p.as_ref().to_path_buf()).collect())
        });
        true
    }

    pub fn files() -> Option<Vec<PathBuf>> {
        FAKE.with(|f| f.borrow().clone())
    }

    pub fn clear() {
        FAKE.with(|f| *f.borrow_mut() = None);
    }
}
