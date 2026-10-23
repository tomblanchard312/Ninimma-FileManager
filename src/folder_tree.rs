// Ninimma modification: independent project branding and integration.
//! The folder tree in the navigation pane: which folders are expanded,
//! and their subfolders, listed in the background so a slow drive or
//! share never holds up the window.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A folder's subfolders, as far as known.
#[derive(Clone, Debug, PartialEq)]
pub enum Children {
    Loading,
    /// Name and path, in natural order.
    Loaded(Vec<(String, PathBuf)>),
    /// Couldn't be read (permissions, gone).
    Failed,
}

#[derive(Default)]
pub struct FolderTree {
    expanded: HashSet<PathBuf>,
    children: Arc<Mutex<HashMap<PathBuf, Children>>>,
}

impl FolderTree {
    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Expands or collapses `path`; expanding lists it afresh.
    pub fn toggle(&mut self, path: &Path, show_hidden: bool, repaint: impl Fn() + Send + 'static) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
            self.load(path, show_hidden, repaint);
        }
    }

    /// Expands `path` (listing it if it hasn't been), leaving it open.
    pub fn expand(&mut self, path: &Path, show_hidden: bool, repaint: impl Fn() + Send + 'static) {
        if self.expanded.insert(path.to_path_buf())
            && !self.children.lock().unwrap().contains_key(path)
        {
            self.load(path, show_hidden, repaint);
        }
    }

    pub fn children(&self, path: &Path) -> Option<Children> {
        self.children.lock().unwrap().get(path).cloned()
    }

    /// Lists `path` again if it's been listed (after a change on disk).
    pub fn refresh(&mut self, path: &Path, show_hidden: bool, repaint: impl Fn() + Send + 'static) {
        if self.children.lock().unwrap().contains_key(path) {
            self.load(path, show_hidden, repaint);
        }
    }

    /// Forgets every listing (e.g. hidden folders were turned on or off);
    /// expanded folders are listed again.
    pub fn reload_all(&mut self, show_hidden: bool, repaint: impl Fn() + Send + Sync + 'static) {
        self.children.lock().unwrap().clear();
        let repaint = Arc::new(repaint);
        for path in self.expanded.clone() {
            let r = repaint.clone();
            self.load(&path, show_hidden, move || r());
        }
    }

    fn load(&mut self, path: &Path, show_hidden: bool, repaint: impl Fn() + Send + 'static) {
        let children = self.children.clone();
        let path = path.to_path_buf();
        children
            .lock()
            .unwrap()
            .entry(path.clone())
            .or_insert(Children::Loading);
        let _ = std::thread::Builder::new()
            .name("folder-tree".into())
            .spawn(move || {
                let listed = list_subfolders(&path, show_hidden);
                children.lock().unwrap().insert(path, listed);
                repaint();
            });
    }
}

/// A folder's subfolders: real folders only (links aren't followed, so the
/// tree can't loop), hidden ones only if asked, in natural order.
pub fn list_subfolders(path: &Path, show_hidden: bool) -> Children {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Children::Failed;
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
        .filter(|(name, path)| show_hidden || !is_hidden(name, path))
        .collect();
    out.sort_by(|a, b| crate::ui::natural_cmp(&a.0, &b.0));
    Children::Loaded(out)
}

fn is_hidden(name: &str, _path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if std::fs::metadata(_path).is_ok_and(|m| m.file_attributes() & 0x2 != 0) {
            return true;
        }
    }
    name.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn subfolders_are_listed_in_order_without_files_hidden_or_links() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["Photos 10", "Photos 2", ".cache", "Docs"] {
            std::fs::create_dir(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("file.txt"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        let names = |c: Children| match c {
            Children::Loaded(v) => v.into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            names(list_subfolders(dir.path(), false)),
            ["Docs", "Photos 2", "Photos 10"]
        );
        assert!(names(list_subfolders(dir.path(), true)).contains(&".cache".to_string()));
        assert_eq!(
            list_subfolders(&dir.path().join("missing"), false),
            Children::Failed
        );
    }

    #[test]
    fn expanding_lists_in_the_background_and_collapsing_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Sub")).unwrap();
        let mut tree = FolderTree::default();
        tree.toggle(dir.path(), false, || {});
        assert!(tree.is_expanded(dir.path()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(tree.children(dir.path()), Some(Children::Loaded(_))) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        tree.toggle(dir.path(), false, || {});
        assert!(!tree.is_expanded(dir.path()));
    }
}
