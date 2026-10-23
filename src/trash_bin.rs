// Ninimma modification: independent project branding and integration.
//! The Recycle Bin / Trash: list what's in it, restore items to where they
//! were, delete them for good. Linux, FreeBSD and Windows (the `trash`
//! crate can't list the macOS Trash). Tests use a stand-in, so they never
//! read or change the real trash.

use std::ffi::OsString;
use std::path::PathBuf;

/// One item in the trash.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// The trash's own identifier for it.
    pub id: OsString,
    pub name: String,
    /// The folder it was deleted from.
    pub from: PathBuf,
    /// When, in seconds since 1970.
    pub deleted: i64,
}

impl Item {
    pub fn original_path(&self) -> PathBuf {
        self.from.join(&self.name)
    }
}

/// Whether this platform's trash can be browsed.
pub const SUPPORTED: bool = cfg!(any(test, windows, all(unix, not(target_os = "macos"))));

/// Everything in the trash, most recently deleted first.
pub fn list() -> Result<Vec<Item>, String> {
    let mut items = imp::list()?;
    items.sort_by(|a, b| b.deleted.cmp(&a.deleted).then_with(|| a.name.cmp(&b.name)));
    Ok(items)
}

/// Puts items back where they were deleted from. Items whose place is
/// taken again stay in the trash, and are named in the errors.
pub fn restore(ids: &[OsString]) -> Vec<String> {
    let mut errors = Vec::new();
    let mut todo = Vec::new();
    match list() {
        Ok(items) => {
            for item in items.into_iter().filter(|i| ids.contains(&i.id)) {
                if item.original_path().symlink_metadata().is_ok() {
                    errors.push(format!(
                        "\"{}\" wasn't restored: {} already has an item with that name",
                        item.name,
                        item.from.display()
                    ));
                } else {
                    todo.push(item.id);
                }
            }
        }
        Err(e) => errors.push(e),
    }
    if !todo.is_empty() {
        if let Err(e) = imp::restore(&todo) {
            errors.push(e);
        }
    }
    errors
}

/// Deletes items from the trash for good.
pub fn purge(ids: &[OsString]) -> Result<(), String> {
    imp::purge(ids)
}

#[cfg(all(not(test), any(windows, all(unix, not(target_os = "macos")))))]
mod imp {
    use super::Item;
    use std::ffi::OsString;
    use trash::os_limited;

    pub fn list() -> Result<Vec<Item>, String> {
        let items = os_limited::list().map_err(|e| format!("Couldn't read the trash: {e}"))?;
        Ok(items
            .into_iter()
            .map(|i| Item {
                name: i.name.to_string_lossy().to_string(),
                from: i.original_parent,
                deleted: i.time_deleted,
                id: i.id,
            })
            .collect())
    }

    fn matching(ids: &[OsString]) -> Result<Vec<trash::TrashItem>, String> {
        let items = os_limited::list().map_err(|e| format!("Couldn't read the trash: {e}"))?;
        Ok(items.into_iter().filter(|i| ids.contains(&i.id)).collect())
    }

    pub fn restore(ids: &[OsString]) -> Result<(), String> {
        os_limited::restore_all(matching(ids)?).map_err(|e| format!("Couldn't restore: {e}"))
    }

    pub fn purge(ids: &[OsString]) -> Result<(), String> {
        os_limited::purge_all(matching(ids)?).map_err(|e| format!("Couldn't delete: {e}"))
    }
}

#[cfg(all(not(test), not(any(windows, all(unix, not(target_os = "macos"))))))]
mod imp {
    use super::Item;
    use std::ffi::OsString;

    const UNSUPPORTED: &str = "Browsing the Trash isn't supported on this platform yet";

    pub fn list() -> Result<Vec<Item>, String> {
        Err(UNSUPPORTED.into())
    }

    pub fn restore(_ids: &[OsString]) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub fn purge(_ids: &[OsString]) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
}

/// A stand-in trash: a folder per test thread, holding the files.
#[cfg(test)]
pub mod imp {
    use super::Item;
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    thread_local! {
        static FAKE: RefCell<Vec<(Item, PathBuf)>> = const { RefCell::new(Vec::new()) };
        static STORE: tempfile::TempDir = tempfile::tempdir().unwrap();
    }

    /// Moves `path` into the stand-in trash, as deleting it would.
    pub fn put(path: &Path, deleted: i64) {
        let n = FAKE.with(|f| f.borrow().len());
        let stored = STORE.with(|s| s.path().join(n.to_string()));
        std::fs::rename(path, &stored).unwrap();
        let item = Item {
            id: OsString::from(n.to_string()),
            name: path.file_name().unwrap().to_string_lossy().to_string(),
            from: path.parent().unwrap().to_path_buf(),
            deleted,
        };
        FAKE.with(|f| f.borrow_mut().push((item, stored)));
    }

    pub fn list() -> Result<Vec<Item>, String> {
        Ok(FAKE.with(|f| f.borrow().iter().map(|(i, _)| i.clone()).collect()))
    }

    pub fn restore(ids: &[OsString]) -> Result<(), String> {
        FAKE.with(|f| {
            f.borrow_mut().retain(|(item, stored)| {
                if !ids.contains(&item.id) {
                    return true;
                }
                std::fs::rename(stored, item.original_path()).unwrap();
                false
            })
        });
        Ok(())
    }

    pub fn purge(ids: &[OsString]) -> Result<(), String> {
        FAKE.with(|f| {
            f.borrow_mut().retain(|(item, stored)| {
                if !ids.contains(&item.id) {
                    return true;
                }
                let _ = std::fs::remove_dir_all(stored).or_else(|_| std::fs::remove_file(stored));
                false
            })
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_to_where_it_was_unless_the_name_is_taken_again() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        imp::put(&a, 100);
        imp::put(&b, 200);

        let items = list().unwrap();
        assert_eq!(items[0].name, "b.txt", "newest first");

        // A new a.txt took the old one's place.
        std::fs::write(&a, "new").unwrap();
        let ids: Vec<_> = items.iter().map(|i| i.id.clone()).collect();
        let errors = restore(&ids);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("a.txt"));
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "new",
            "never overwritten"
        );
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b");
        assert_eq!(list().unwrap().len(), 1, "a.txt stays in the trash");

        purge(&[list().unwrap()[0].id.clone()]).unwrap();
        assert!(list().unwrap().is_empty());
    }
}
