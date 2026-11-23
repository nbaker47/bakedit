//! The centre pane: an ordered set of tabs, which one is in front, the preview tab, the
//! closed-tab stack and the back/forward history. Zed's rules:
//!
//! * A new tab goes right after the active one and comes to the front.
//! * There is at most one preview tab. Opening another preview reuses its place; editing it
//!   or opening it for real makes it permanent.
//! * Closing the active tab goes back to the tab that was in front before it.

use std::path::{Path, PathBuf};

use crate::view::Item;

/// A place a jump started from or went to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    pub column: u32,
}

pub struct Slot {
    pub item: Box<dyn Item>,
    pub preview: bool,
    id: u64,
}

/// Jumps shorter than this many lines within one file are not worth a history entry.
const NEARBY_LINES: u32 = 5;
const HISTORY_LIMIT: usize = 200;
const CLOSED_LIMIT: usize = 50;

#[derive(Default)]
pub struct Pane {
    slots: Vec<Slot>,
    active: usize,
    next_id: u64,
    /// Slot ids in the order they were last in front, most recent last.
    recency: Vec<u64>,
    closed: Vec<Location>,
    back: Vec<Location>,
    forward: Vec<Location>,
    /// Leftmost tab drawn; kept here so the tab bar scrolls smoothly between frames.
    pub first_tab: usize,
}

impl Pane {
    pub fn new() -> Pane {
        Pane::default()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    pub fn active_index(&self) -> Option<usize> {
        if self.slots.is_empty() {
            None
        } else {
            Some(self.active)
        }
    }

    pub fn active(&self) -> Option<&dyn Item> {
        self.slots.get(self.active).map(|slot| slot.item.as_ref())
    }

    pub fn active_mut(&mut self) -> Option<&mut (dyn Item + 'static)> {
        self.slots.get_mut(self.active).map(|slot| slot.item.as_mut())
    }

    pub fn item(&self, index: usize) -> Option<&dyn Item> {
        self.slots.get(index).map(|slot| slot.item.as_ref())
    }

    pub fn item_mut(&mut self, index: usize) -> Option<&mut (dyn Item + 'static)> {
        self.slots.get_mut(index).map(|slot| slot.item.as_mut())
    }

    pub fn items_mut(&mut self) -> impl Iterator<Item = &mut (dyn Item + 'static)> {
        self.slots.iter_mut().map(|slot| slot.item.as_mut())
    }

    /// The tab showing `path`, if one is open.
    pub fn find_path(&self, path: &Path) -> Option<usize> {
        self.slots.iter().position(|slot| slot.item.path() == Some(path))
    }

    fn touch(&mut self, index: usize) {
        if let Some(id) = self.slots.get(index).map(|slot| slot.id) {
            self.recency.retain(|other| *other != id);
            self.recency.push(id);
        }
    }

    /// Add a tab and bring it to the front. A preview tab takes the place of the existing
    /// preview tab if there is one; any other tab goes right after the active one. Returns
    /// the new tab's index and the item that was replaced, if any.
    pub fn add(&mut self, item: Box<dyn Item>, preview: bool) -> (usize, Option<Box<dyn Item>>) {
        let id = self.next_id;
        self.next_id += 1;
        let existing_preview = self.slots.iter().position(|slot| slot.preview);
        let (index, replaced) = match existing_preview {
            Some(index) if preview => {
                let old = std::mem::replace(&mut self.slots[index], Slot { item, preview: true, id });
                self.recency.retain(|other| *other != old.id);
                (index, Some(old.item))
            }
            _ => {
                let index = if self.slots.is_empty() { 0 } else { self.active + 1 };
                self.slots.insert(index, Slot { item, preview, id });
                (index, None)
            }
        };
        self.active = index;
        self.touch(index);
        (index, replaced)
    }

    /// Bring a tab to the front.
    pub fn activate(&mut self, index: usize) -> bool {
        if index >= self.slots.len() {
            return false;
        }
        self.active = index;
        self.touch(index);
        true
    }

    pub fn activate_next(&mut self) {
        if !self.slots.is_empty() {
            self.activate((self.active + 1) % self.slots.len());
        }
    }

    pub fn activate_previous(&mut self) {
        if !self.slots.is_empty() {
            self.activate((self.active + self.slots.len() - 1) % self.slots.len());
        }
    }

    pub fn activate_last(&mut self) {
        if !self.slots.is_empty() {
            self.activate(self.slots.len() - 1);
        }
    }

    /// Tabs in the order they were last in front, most recent first (for the tab switcher).
    pub fn by_recency(&self) -> Vec<usize> {
        let mut order: Vec<usize> = self
            .recency
            .iter()
            .rev()
            .filter_map(|id| self.slots.iter().position(|slot| slot.id == *id))
            .collect();
        for index in 0..self.slots.len() {
            if !order.contains(&index) {
                order.push(index);
            }
        }
        order
    }

    /// Make a preview tab permanent.
    pub fn keep(&mut self, index: usize) {
        if let Some(slot) = self.slots.get_mut(index) {
            slot.preview = false;
        }
    }

    pub fn is_preview(&self, index: usize) -> bool {
        self.slots.get(index).is_some_and(|slot| slot.preview)
    }

    /// Close a tab and return its item. The tab that comes to the front is the one that was
    /// in front most recently. A closed file is remembered for "reopen closed tab".
    pub fn close(&mut self, index: usize) -> Option<Box<dyn Item>> {
        if index >= self.slots.len() {
            return None;
        }
        let was_active = index == self.active;
        let slot = self.slots.remove(index);
        self.recency.retain(|id| *id != slot.id);
        if let Some(path) = slot.item.path() {
            let (line, column) = slot.item.position().unwrap_or((1, 1));
            self.closed.push(Location { path: path.to_path_buf(), line, column });
            if self.closed.len() > CLOSED_LIMIT {
                self.closed.remove(0);
            }
        }
        if self.slots.is_empty() {
            self.active = 0;
            self.first_tab = 0;
        } else if was_active {
            let previous = self.recency.last().and_then(|id| self.slots.iter().position(|slot| slot.id == *id));
            self.active = previous.unwrap_or_else(|| index.min(self.slots.len() - 1));
            self.touch(self.active);
        } else if index < self.active {
            self.active -= 1;
        }
        Some(slot.item)
    }

    /// The most recently closed file, removed from the stack. Files that are open again
    /// are skipped.
    pub fn pop_closed(&mut self) -> Option<Location> {
        while let Some(location) = self.closed.pop() {
            if self.find_path(&location.path).is_none() {
                return Some(location);
            }
        }
        None
    }

    /// Move the active tab one place left or right, keeping it active.
    pub fn swap_active(&mut self, right: bool) {
        if self.slots.len() < 2 {
            return;
        }
        let target = if right { self.active + 1 } else { self.active.wrapping_sub(1) };
        if target < self.slots.len() {
            self.slots.swap(self.active, target);
            self.active = target;
        }
    }

    /// The tabs a bulk close would close, highest index first so they can be closed in
    /// order without the indices shifting.
    pub fn close_candidates(&self, which: CloseSet) -> Vec<usize> {
        let active = self.active;
        (0..self.slots.len())
            .rev()
            .filter(|&index| match which {
                CloseSet::All => true,
                CloseSet::Others => index != active,
                CloseSet::ToTheLeft => index < active,
                CloseSet::ToTheRight => index > active,
                CloseSet::Clean => !self.slots[index].item.is_dirty(),
            })
            .collect()
    }

    // ---- back / forward ----------------------------------------------------------------

    /// Remember where a jump started, so "go back" can return there. Small moves within a
    /// file are not recorded.
    pub fn record_jump(&mut self, from: Location) {
        if let Some(last) = self.back.last() {
            if last.path == from.path && last.line.abs_diff(from.line) < NEARBY_LINES {
                return;
            }
        }
        self.back.push(from);
        if self.back.len() > HISTORY_LIMIT {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    /// Where "go back" should take the user. `current` is where they are now; it becomes
    /// the place "go forward" returns to.
    pub fn go_back(&mut self, current: Option<Location>) -> Option<Location> {
        let target = self.back.pop()?;
        if let Some(current) = current {
            self.forward.push(current);
        }
        Some(target)
    }

    pub fn go_forward(&mut self, current: Option<Location>) -> Option<Location> {
        let target = self.forward.pop()?;
        if let Some(current) = current {
            self.back.push(current);
        }
        Some(target)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseSet {
    All,
    Others,
    ToTheLeft,
    ToTheRight,
    /// Tabs with no unsaved changes.
    Clean,
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::chrome::Tab;
    use crate::ui::Canvas;
    use crate::view::{Context, Cursor, Cx, RenderCx, View};
    use bakedit_core::{Action, KeyEvent};
    use ratatui::layout::Rect;
    use std::any::Any;

    /// A stand-in item: a name, optionally a path, and a dirty flag.
    pub struct FakeItem {
        pub name: String,
        pub path: Option<PathBuf>,
        pub dirty: bool,
        pub line: u32,
        pub kind: &'static str,
        pub saves: usize,
        /// Actions this item claims, and a log of the ones it was offered.
        pub handles: Vec<&'static str>,
        pub seen: Vec<String>,
    }

    impl FakeItem {
        pub fn file(path: &str) -> Box<FakeItem> {
            let name = Path::new(path).file_name().unwrap().to_string_lossy().into_owned();
            Box::new(FakeItem {
                name,
                path: Some(PathBuf::from(path)),
                dirty: false,
                line: 1,
                kind: "editor",
                saves: 0,
                handles: Vec::new(),
                seen: Vec::new(),
            })
        }

        pub fn dirty(path: &str) -> Box<FakeItem> {
            let mut item = FakeItem::file(path);
            item.dirty = true;
            item
        }

        pub fn untitled(name: &str) -> Box<FakeItem> {
            let mut item = FakeItem::file("/untitled");
            item.name = name.to_string();
            item.path = None;
            item
        }
    }

    impl View for FakeItem {
        fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
            canvas.fill(area, cx.theme.editor_bg);
            canvas.text(area.x, area.y, &self.name, crate::ui::Style::fg(cx.theme.text), area.width);
            None
        }
        fn contexts(&self, out: &mut Vec<Context>) {
            out.push(Context::new("Fake"));
        }
        fn handle_action(&mut self, action: &Action, _: &mut Cx<'_>) -> bool {
            self.seen.push(action.name.clone());
            self.handles.contains(&action.name.as_str())
        }
        fn handle_key(&mut self, key: &KeyEvent, _: &mut Cx<'_>) -> bool {
            if let Some(text) = &key.text {
                self.seen.push(format!("typed {text}"));
            }
            true
        }
        fn focus_changed(&mut self, focused: bool, _: &mut Cx<'_>) {
            self.seen.push(format!("focus {focused}"));
        }
    }

    impl Item for FakeItem {
        fn kind(&self) -> &'static str {
            self.kind
        }
        fn save(&mut self, _: bool, _: &mut Cx<'_>) -> Result<(), String> {
            if self.name.contains("readonly") {
                return Err(format!("{} is read-only", self.name));
            }
            self.saves += 1;
            self.dirty = false;
            Ok(())
        }
        fn tab(&self, _: &RenderCx<'_>) -> Tab {
            Tab { title: self.name.clone(), dirty: self.dirty, ..Default::default() }
        }
        fn path(&self) -> Option<&Path> {
            self.path.as_deref()
        }
        fn is_dirty(&self) -> bool {
            self.dirty
        }
        fn position(&self) -> Option<(u32, u32)> {
            Some((self.line, 1))
        }
        fn go_to(&mut self, line: u32, _: Option<u32>) {
            self.line = line;
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeItem;
    use super::*;

    fn names(pane: &Pane) -> Vec<String> {
        pane.slots().iter().map(|slot| slot.item.path().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).unwrap_or_default()).collect()
    }

    fn active_name(pane: &Pane) -> String {
        names(pane)[pane.active_index().unwrap()].clone()
    }

    fn pane_with(paths: &[&str]) -> Pane {
        let mut pane = Pane::new();
        for path in paths {
            pane.add(FakeItem::file(path), false);
        }
        pane
    }

    fn location(path: &str, line: u32) -> Location {
        Location { path: PathBuf::from(path), line, column: 1 }
    }

    #[test]
    fn new_tabs_go_after_the_active_one() {
        let mut pane = pane_with(&["/a", "/b", "/c"]);
        assert_eq!(names(&pane), ["a", "b", "c"]);
        assert_eq!(active_name(&pane), "c");
        pane.activate(0);
        pane.add(FakeItem::file("/d"), false);
        assert_eq!(names(&pane), ["a", "d", "b", "c"]);
        assert_eq!(active_name(&pane), "d");
        assert_eq!(pane.find_path(Path::new("/b")), Some(2));
        assert_eq!(pane.find_path(Path::new("/zzz")), None);
    }

    #[test]
    fn one_preview_tab_is_reused() {
        let mut pane = pane_with(&["/a"]);
        let (index, replaced) = pane.add(FakeItem::file("/p1"), true);
        assert_eq!((index, replaced.is_none()), (1, true));
        assert!(pane.is_preview(1));
        // The next preview takes its place even when another tab is active.
        pane.activate(0);
        let (index, replaced) = pane.add(FakeItem::file("/p2"), true);
        assert_eq!(index, 1);
        assert_eq!(replaced.unwrap().path(), Some(Path::new("/p1")));
        assert_eq!(names(&pane), ["a", "p2"]);
        assert_eq!(active_name(&pane), "p2");
        // Kept, it stops being replaced.
        pane.keep(1);
        pane.add(FakeItem::file("/p3"), true);
        assert_eq!(names(&pane), ["a", "p2", "p3"]);
        // A permanent tab never replaces the preview.
        pane.add(FakeItem::file("/x"), false);
        assert_eq!(names(&pane), ["a", "p2", "p3", "x"]);
        assert!(pane.is_preview(2));
    }

    #[test]
    fn closing_returns_to_the_previously_active_tab() {
        let mut pane = pane_with(&["/a", "/b", "/c", "/d"]);
        pane.activate(0);
        pane.activate(3);
        let closed = pane.close(3).unwrap();
        assert_eq!(closed.path(), Some(Path::new("/d")));
        assert_eq!(active_name(&pane), "a", "back to the tab used before, not the neighbour");
        // Closing a tab left of the active one keeps the same tab active.
        pane.activate(2);
        pane.close(0);
        assert_eq!(active_name(&pane), "c");
        assert_eq!(names(&pane), ["b", "c"]);
        pane.close(1);
        pane.close(0);
        assert!(pane.is_empty());
        assert_eq!(pane.active_index(), None);
        assert!(pane.close(0).is_none());
        assert!(pane.active().is_none());
    }

    #[test]
    fn reopening_closed_tabs_newest_first() {
        let mut pane = pane_with(&["/a", "/b"]);
        pane.add(FakeItem::untitled("scratch"), false);
        pane.close(2);
        pane.close(1);
        pane.close(0);
        assert_eq!(pane.pop_closed(), Some(location("/a", 1)));
        assert_eq!(pane.pop_closed(), Some(location("/b", 1)));
        assert_eq!(pane.pop_closed(), None, "an untitled tab has no file to reopen");
        // A file that has been opened again is skipped.
        pane.add(FakeItem::file("/a"), false);
        pane.close(0);
        pane.add(FakeItem::file("/a"), false);
        assert_eq!(pane.pop_closed(), None);
    }

    #[test]
    fn cycling_and_recency() {
        let mut pane = pane_with(&["/a", "/b", "/c"]);
        pane.activate_next();
        assert_eq!(active_name(&pane), "a", "wraps");
        pane.activate_previous();
        assert_eq!(active_name(&pane), "c");
        pane.activate(1);
        assert_eq!(pane.by_recency(), vec![1, 2, 0]);
        pane.activate_last();
        assert_eq!(active_name(&pane), "c");
        assert!(!pane.activate(9));
    }

    #[test]
    fn swapping_moves_the_active_tab() {
        let mut pane = pane_with(&["/a", "/b", "/c"]);
        pane.activate(1);
        pane.swap_active(true);
        assert_eq!(names(&pane), ["a", "c", "b"]);
        assert_eq!(active_name(&pane), "b");
        pane.swap_active(true);
        assert_eq!(names(&pane), ["a", "c", "b"], "already at the end");
        pane.swap_active(false);
        pane.swap_active(false);
        pane.swap_active(false);
        assert_eq!(names(&pane), ["b", "a", "c"]);
    }

    #[test]
    fn bulk_close_sets() {
        let mut pane = Pane::new();
        pane.add(FakeItem::file("/a"), false);
        pane.add(FakeItem::dirty("/b"), false);
        pane.add(FakeItem::file("/c"), false);
        pane.add(FakeItem::file("/d"), false);
        pane.activate(1);
        assert_eq!(pane.close_candidates(CloseSet::All), vec![3, 2, 1, 0]);
        assert_eq!(pane.close_candidates(CloseSet::Others), vec![3, 2, 0]);
        assert_eq!(pane.close_candidates(CloseSet::ToTheLeft), vec![0]);
        assert_eq!(pane.close_candidates(CloseSet::ToTheRight), vec![3, 2]);
        assert_eq!(pane.close_candidates(CloseSet::Clean), vec![3, 2, 0]);
    }

    #[test]
    fn back_and_forward() {
        let mut pane = Pane::new();
        assert_eq!(pane.go_back(None), None);
        pane.record_jump(location("/a", 10));
        pane.record_jump(location("/a", 12));
        pane.record_jump(location("/a", 80));
        pane.record_jump(location("/b", 80));
        // /a:12 was too close to /a:10 to keep.
        assert_eq!(pane.go_back(Some(location("/c", 1))), Some(location("/b", 80)));
        assert_eq!(pane.go_back(Some(location("/b", 80))), Some(location("/a", 80)));
        assert_eq!(pane.go_forward(Some(location("/a", 80))), Some(location("/b", 80)));
        assert_eq!(pane.go_forward(Some(location("/b", 80))), Some(location("/c", 1)));
        assert_eq!(pane.go_forward(None), None);
        // A new jump discards the forward list.
        pane.go_back(Some(location("/c", 1)));
        pane.record_jump(location("/z", 5));
        assert_eq!(pane.go_forward(None), None);
    }
}
