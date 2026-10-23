// Ninimma modification: independent project branding and integration.
//! Explorer-style multi-selection over the currently displayed item list.
//!
//! Selection is stored by path so it survives re-sorting and auto-refresh;
//! `anchor` is where Shift+click/Shift+arrow ranges extend from and `focus`
//! is the item the keyboard cursor is on.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Selection {
    selected: HashSet<PathBuf>,
    anchor: Option<PathBuf>,
    focus: Option<PathBuf>,
}

impl Selection {
    pub fn contains(&self, path: &Path) -> bool {
        self.selected.contains(path)
    }

    pub fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub fn focus(&self) -> Option<&Path> {
        self.focus.as_deref()
    }

    /// Selected paths in display order.
    pub fn ordered(&self, items: &[PathBuf]) -> Vec<PathBuf> {
        items
            .iter()
            .filter(|p| self.selected.contains(*p))
            .cloned()
            .collect()
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.focus = None;
    }

    pub fn select_only(&mut self, path: &Path) {
        self.selected.clear();
        self.selected.insert(path.to_path_buf());
        self.anchor = Some(path.to_path_buf());
        self.focus = Some(path.to_path_buf());
    }

    pub fn select_all(&mut self, items: &[PathBuf]) {
        self.selected = items.iter().cloned().collect();
    }

    /// Select exactly the items that aren't selected now.
    pub fn invert(&mut self, items: &[PathBuf]) {
        self.selected = items
            .iter()
            .filter(|p| !self.selected.contains(*p))
            .cloned()
            .collect();
    }

    /// Replace the selection wholesale (rubber-band selection).
    pub fn set(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.selected = paths.into_iter().collect();
    }

    pub fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.selected.iter()
    }

    /// Apply a mouse click on `items[index]` with the given modifier keys.
    pub fn click(&mut self, items: &[PathBuf], index: usize, ctrl: bool, shift: bool) {
        let Some(path) = items.get(index) else { return };
        match (ctrl, shift) {
            (false, false) => self.select_only(path),
            (true, false) => {
                if !self.selected.remove(path) {
                    self.selected.insert(path.clone());
                }
                self.anchor = Some(path.clone());
                self.focus = Some(path.clone());
            }
            (_, true) => {
                let anchor = self.anchor_index(items).unwrap_or(index);
                if !ctrl {
                    self.selected.clear();
                }
                let (lo, hi) = (anchor.min(index), anchor.max(index));
                self.selected.extend(items[lo..=hi].iter().cloned());
                self.focus = Some(path.clone());
                if self.anchor.is_none() {
                    self.anchor = Some(path.clone());
                }
            }
        }
    }

    /// Right-clicking an unselected item selects just it, like Explorer;
    /// right-clicking inside the selection keeps the selection.
    pub fn right_click(&mut self, path: &Path) {
        if !self.selected.contains(path) {
            self.select_only(path);
        }
    }

    /// Move the keyboard focus by `delta` items (e.g. ±1 or ±columns).
    /// With `shift`, extends the selection from the anchor.
    pub fn move_focus(&mut self, items: &[PathBuf], delta: isize, shift: bool) -> Option<PathBuf> {
        if items.is_empty() {
            return None;
        }
        let current = self.focus_index(items);
        let target = match current {
            Some(i) => (i as isize + delta).clamp(0, items.len() as isize - 1) as usize,
            None if delta < 0 => items.len() - 1,
            None => 0,
        };
        self.move_focus_to(items, target, shift)
    }

    pub fn move_focus_to(
        &mut self,
        items: &[PathBuf],
        index: usize,
        shift: bool,
    ) -> Option<PathBuf> {
        let index = index.min(items.len().checked_sub(1)?);
        if shift {
            self.click(items, index, false, true);
        } else {
            self.select_only(&items[index]);
        }
        Some(items[index].clone())
    }

    /// Drop selected paths that no longer exist in the listing (after refresh).
    pub fn retain_existing(&mut self, items: &[PathBuf]) {
        let present: HashSet<&PathBuf> = items.iter().collect();
        self.selected.retain(|p| present.contains(p));
        if self.anchor.as_ref().is_some_and(|a| !present.contains(a)) {
            self.anchor = None;
        }
        if self.focus.as_ref().is_some_and(|f| !present.contains(f)) {
            self.focus = None;
        }
    }

    fn anchor_index(&self, items: &[PathBuf]) -> Option<usize> {
        let a = self.anchor.as_ref()?;
        items.iter().position(|p| p == a)
    }

    fn focus_index(&self, items: &[PathBuf]) -> Option<usize> {
        let f = self.focus.as_ref()?;
        items.iter().position(|p| p == f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<PathBuf> {
        (0..n).map(|i| PathBuf::from(format!("/f{i}"))).collect()
    }

    fn selected(s: &Selection, items: &[PathBuf]) -> Vec<usize> {
        (0..items.len())
            .filter(|&i| s.contains(&items[i]))
            .collect()
    }

    #[test]
    fn click_ctrl_shift_behave_like_explorer() {
        let it = items(10);
        let mut s = Selection::default();
        s.click(&it, 2, false, false);
        assert_eq!(selected(&s, &it), [2]);
        s.click(&it, 5, false, true); // shift: range from anchor
        assert_eq!(selected(&s, &it), [2, 3, 4, 5]);
        s.click(&it, 0, false, true); // shift again: range re-pivots on same anchor
        assert_eq!(selected(&s, &it), [0, 1, 2]);
        s.click(&it, 8, true, false); // ctrl: toggle add, moves anchor
        assert_eq!(selected(&s, &it), [0, 1, 2, 8]);
        s.click(&it, 1, true, false); // ctrl: toggle remove
        assert_eq!(selected(&s, &it), [0, 2, 8]);
        s.click(&it, 3, true, true); // ctrl+shift: add range from anchor (1)
        assert_eq!(selected(&s, &it), [0, 1, 2, 3, 8]);
    }

    #[test]
    fn keyboard_navigation_and_extend() {
        let it = items(10);
        let mut s = Selection::default();
        s.move_focus(&it, 1, false);
        assert_eq!(selected(&s, &it), [0]);
        s.move_focus(&it, 4, false);
        assert_eq!(selected(&s, &it), [4]);
        s.move_focus(&it, 2, true);
        assert_eq!(selected(&s, &it), [4, 5, 6]);
        s.move_focus(&it, 100, false);
        assert_eq!(selected(&s, &it), [9]);
        s.move_focus(&it, -100, true);
        assert_eq!(selected(&s, &it), (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn shift_moves_keep_a_contiguous_range_that_shrinks_toward_the_anchor() {
        let it = items(10);
        let mut s = Selection::default();
        s.click(&it, 3, false, false);
        for _ in 0..3 {
            s.move_focus(&it, 1, true);
        }
        assert_eq!(selected(&s, &it), [3, 4, 5, 6]);
        s.move_focus(&it, -1, true);
        assert_eq!(selected(&s, &it), [3, 4, 5]);
        // Crossing the anchor flips the range to the other side.
        s.move_focus(&it, -4, true);
        assert_eq!(selected(&s, &it), [1, 2, 3]);
        // Home/End and page-sized jumps extend from the same anchor.
        s.move_focus(&it, isize::MAX / 2, true);
        assert_eq!(selected(&s, &it), [3, 4, 5, 6, 7, 8, 9]);
        s.move_focus(&it, isize::MIN / 2, true);
        assert_eq!(selected(&s, &it), [0, 1, 2, 3]);
        s.move_focus(&it, 5, true);
        assert_eq!(selected(&s, &it), [3, 4, 5]);
        assert_eq!(s.focus(), Some(it[5].as_path()));
    }

    #[test]
    fn right_click_preserves_existing_selection() {
        let it = items(5);
        let mut s = Selection::default();
        s.select_all(&it);
        s.right_click(&it[3]);
        assert_eq!(s.len(), 5);
        s.select_only(&it[0]);
        s.right_click(&it[3]);
        assert_eq!(selected(&s, &it), [3]);
    }

    #[test]
    fn refresh_drops_deleted_items() {
        let it = items(5);
        let mut s = Selection::default();
        s.select_all(&it);
        s.retain_existing(&it[2..]);
        assert_eq!(s.len(), 3);
        assert_eq!(s.ordered(&it), it[2..].to_vec());
    }
}
