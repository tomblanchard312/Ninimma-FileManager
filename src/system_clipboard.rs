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

// egui-winit otherwise consumes Ctrl+V even when a file-only clipboard
// produces no text event, preventing the file manager's paste fallback.
pub fn prepare_input(ctx: &egui::Context, input: &mut egui::RawInput) {
    translate_keys(
        &mut input.events,
        ctx.os(),
        ctx.egui_wants_keyboard_input(),
        imp::text,
    );
}

fn translate_keys(
    events: &mut [egui::Event],
    os: egui::os::OperatingSystem,
    typing: bool,
    mut text: impl FnMut() -> Option<String>,
) {
    let windows = os == egui::os::OperatingSystem::Windows;
    use egui::{Event, Key};
    for event in events {
        let Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } = event
        else {
            continue;
        };
        let command = modifiers.command_only();
        if *key == Key::Copy
            || (command && *key == Key::C)
            || (windows && modifiers.ctrl && *key == Key::Insert)
        {
            *event = Event::Copy;
        } else if *key == Key::Cut
            || (command && *key == Key::X)
            || (windows && typing && modifiers.shift && *key == Key::Delete)
        {
            *event = Event::Cut;
        } else if *key == Key::Paste
            || (command && *key == Key::V)
            || (windows && modifiers.shift && *key == Key::Insert)
        {
            if let Some(contents) = text().filter(|s| !s.is_empty()) {
                *event = Event::Paste(contents.replace("\r\n", "\n"));
            }
        }
    }
}

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

    pub fn text() -> Option<String> {
        with(|c| c.get_text().ok())
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

    pub fn text() -> Option<String> {
        None
    }

    pub fn clear() {
        FAKE.with(|f| *f.borrow_mut() = None);
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use egui::{Event, Key, Modifiers};

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn clipboard_keys_keep_text_editing_and_file_only_paste() {
        let mut events = [
            key(Key::C, Modifiers::COMMAND),
            key(Key::X, Modifiers::COMMAND),
            key(Key::V, Modifiers::COMMAND),
        ];
        translate_keys(
            &mut events,
            egui::os::OperatingSystem::Windows,
            false,
            || Some("/tmp/a\r\nb".into()),
        );
        assert_eq!(
            events,
            [Event::Copy, Event::Cut, Event::Paste("/tmp/a\nb".into())]
        );
        for text in [None, Some(String::new())] {
            let original = key(Key::V, Modifiers::COMMAND);
            let mut events = [original.clone()];
            translate_keys(
                &mut events,
                egui::os::OperatingSystem::Windows,
                false,
                || text.clone(),
            );
            assert_eq!(
                events,
                [original],
                "file-only paste must reach the UI fallback"
            );
        }
    }

    #[test]
    fn unrelated_keys_and_key_releases_are_unchanged() {
        let mut released = key(Key::C, Modifiers::COMMAND);
        if let Event::Key { pressed, .. } = &mut released {
            *pressed = false;
        }
        let original = [
            key(Key::C, Modifiers::NONE),
            key(Key::C, Modifiers::COMMAND | Modifiers::SHIFT),
            released,
        ];
        let mut events = original.clone();
        translate_keys(
            &mut events,
            egui::os::OperatingSystem::Windows,
            false,
            || panic!("unrelated keys must not read the clipboard"),
        );
        assert_eq!(events, original);
    }
    #[test]
    fn shift_delete_cuts_text_but_preserves_file_delete_confirmation() {
        let key = key(Key::Delete, Modifiers::SHIFT);
        for typing in [false, true] {
            let mut events = [key.clone()];
            translate_keys(
                &mut events,
                egui::os::OperatingSystem::Windows,
                typing,
                || None,
            );
            assert_eq!(events[0], if typing { Event::Cut } else { key.clone() });
        }
    }
}
