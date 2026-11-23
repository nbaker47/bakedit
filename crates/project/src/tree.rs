//! The model behind the project panel.
//!
//! [`FileTree`] behaves like Zed's project panel with `auto_fold_dirs` off. It holds the
//! flattened list of visible [`Row`]s, the selection and the cut/copy clipboard, and it
//! performs the panel's file operations. It draws nothing.
//!
//! Directories are read from disk only when they become visible, one `read_dir` each, on
//! the calling thread. Nothing here watches the file system: feed the paths of a
//! [`crate::FsBatch`] to [`FileTree::refresh`] and the tree re-reads just the directories
//! those paths touch, keeping what was expanded and what was selected.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::fileops::{self, FileOpError};
use crate::ignore_rules::{
    has_hidden_component, is_always_hidden, is_ignore_file, normalize, resolve, IgnoreRules,
};
use crate::order::compare_entries;

/// The most directories one [`FileTree::expand_all`] call opens. It keeps "expand all" on
/// an enormous directory from freezing the caller; the directories nearest the start are
/// opened first.
pub const EXPAND_ALL_LIMIT: usize = 10_000;

/// Whether a row is a file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A directory, or a symbolic link to one.
    Dir {
        /// Whether its children are shown.
        expanded: bool,
    },
    /// Anything that is not a directory, including links that lead nowhere.
    File,
}

/// One visible line of the project panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Absolute path, starting with the tree's root exactly as it was given. Entries
    /// below a symbolic link keep the path through the link.
    pub path: PathBuf,
    /// The entry's own name, for display. Not valid UTF-8 is shown lossily.
    pub name: String,
    /// Nesting level: 0 for the root, 1 for its children.
    pub depth: usize,
    /// File or directory, and for a directory whether it is expanded.
    pub kind: RowKind,
    /// Matched by an ignore file (or inside a directory that is). The UI dims these.
    pub ignored: bool,
    /// The entry itself is a symbolic link.
    pub symlink: bool,
    /// The project root, always the first row.
    pub is_root: bool,
}

impl Row {
    /// Whether the row is a directory, expanded or not.
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, RowKind::Dir { .. })
    }

    /// Whether the row is a directory whose children are shown.
    pub fn is_expanded(&self) -> bool {
        matches!(self.kind, RowKind::Dir { expanded: true })
    }
}

/// The entries that were cut or copied in the panel, waiting for a paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    /// Absolute paths, in the order they were given.
    pub paths: Vec<PathBuf>,
    /// `true` when a paste moves the entries, `false` when it copies them. The UI
    /// usually dims cut rows.
    pub cut: bool,
}

/// One entry of a loaded directory.
#[derive(Debug, Clone)]
struct Entry {
    name: OsString,
    display: String,
    is_dir: bool,
    ignored: bool,
    symlink: bool,
}

/// The loaded contents of one visible directory.
#[derive(Debug, Default)]
struct Listing {
    entries: Vec<Entry>,
    /// The directory's real location, kept only when it was reached through a symbolic
    /// link. Directories below such a link need it to detect cycles.
    real: Option<PathBuf>,
}

type TrashHandler = Box<dyn Fn(&Path) -> Result<(), FileOpError> + Send>;

/// The file tree of one project directory. See the [module documentation](self).
pub struct FileTree {
    root: PathBuf,
    root_real: PathBuf,
    /// Listings of the directories that are currently visible and expanded.
    listings: HashMap<PathBuf, Listing>,
    /// Every directory the user has open, including ones hidden inside a collapsed
    /// ancestor, which are remembered for when that ancestor opens again.
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
    selected: Option<usize>,
    rules: IgnoreRules,
    clipboard: Option<Clipboard>,
    trash: TrashHandler,
}

impl FileTree {
    /// A tree for the directory `root`, with the root expanded and nothing selected.
    ///
    /// A relative `root` is made absolute against the current directory; symbolic links
    /// in it are kept as written. If the directory cannot be read the tree has just the
    /// root row, and a later [`FileTree::refresh_all`] tries again.
    pub fn new(root: impl AsRef<Path>) -> FileTree {
        let root = normalize(root.as_ref());
        let mut tree = FileTree {
            root_real: fs::canonicalize(&root).unwrap_or_else(|_| root.clone()),
            listings: HashMap::new(),
            expanded: HashSet::from([root.clone()]),
            rows: Vec::new(),
            selected: None,
            rules: IgnoreRules::new(&root),
            clipboard: None,
            trash: Box::new(fileops::trash),
            root,
        };
        tree.sync();
        tree
    }

    /// The project root, absolute, as every row path starts.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The visible rows, top to bottom. The first is always the root.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The index of the selected row, if any. Nothing is selected in a new tree.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The selected row, if any.
    pub fn selected_row(&self) -> Option<&Row> {
        self.selected.and_then(|index| self.rows.get(index))
    }

    /// The absolute path of the selected row, if any.
    pub fn selected_path(&self) -> Option<&Path> {
        self.selected_row().map(|row| row.path.as_path())
    }

    /// The row index of `path` (absolute, or relative to the root) if it is visible.
    pub fn index_of(&self, path: &Path) -> Option<usize> {
        let path = resolve(&self.root, path);
        self.rows.iter().position(|row| row.path == path)
    }

    /// Select the row at `index`. An index past the end selects the last row.
    pub fn select(&mut self, index: usize) {
        self.selected = Some(index.min(self.rows.len().saturating_sub(1)));
    }

    /// Select nothing.
    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// Move the selection one row down, stopping at the last row. With nothing selected,
    /// selects the first row.
    pub fn select_next(&mut self) {
        self.select_page_down(1);
    }

    /// Move the selection one row up, stopping at the first row. With nothing selected,
    /// selects the first row.
    pub fn select_prev(&mut self) {
        self.select_page_up(1);
    }

    /// Select the first row (the root).
    pub fn select_first(&mut self) {
        self.select(0);
    }

    /// Select the last visible row.
    pub fn select_last(&mut self) {
        self.select(usize::MAX);
    }

    /// Move the selection `count` rows down, stopping at the last row. With nothing
    /// selected, selects the first row.
    pub fn select_page_down(&mut self, count: usize) {
        match self.selected {
            Some(index) => self.select(index.saturating_add(count)),
            None => self.select(0),
        }
    }

    /// Move the selection `count` rows up, stopping at the first row. With nothing
    /// selected, selects the first row.
    pub fn select_page_up(&mut self, count: usize) {
        match self.selected {
            Some(index) => self.select(index.saturating_sub(count)),
            None => self.select(0),
        }
    }

    /// On a collapsed directory: expand it. On an expanded directory: move the selection
    /// to its first child (an empty directory keeps the selection). On a file: nothing.
    pub fn expand_selected(&mut self) {
        let Some((index, row)) = self.selection() else {
            return;
        };
        match row.kind {
            RowKind::File => {}
            RowKind::Dir { expanded: false } => {
                self.expand(&row.path);
            }
            RowKind::Dir { expanded: true } => {
                let first_child = self
                    .rows
                    .get(index + 1)
                    .is_some_and(|next| next.depth == row.depth + 1);
                if first_child {
                    self.selected = Some(index + 1);
                }
            }
        }
    }

    /// On an expanded directory: collapse it. On a file or a collapsed directory: move
    /// the selection to the parent directory. On a collapsed root: nothing.
    pub fn collapse_selected(&mut self) {
        let Some((index, row)) = self.selection() else {
            return;
        };
        if row.is_expanded() {
            self.collapse(&row.path);
        } else if let Some(parent) = self.parent_index(index) {
            self.selected = Some(parent);
        }
    }

    /// Expand the selected directory if it is collapsed, collapse it if it is expanded.
    /// Does nothing on a file; opening files is the app's business.
    pub fn toggle_selected(&mut self) {
        let Some((_, row)) = self.selection() else {
            return;
        };
        match row.kind {
            RowKind::File => {}
            RowKind::Dir { expanded: true } => {
                self.collapse(&row.path);
            }
            RowKind::Dir { expanded: false } => {
                self.expand(&row.path);
            }
        }
    }

    /// Expand the selected directory and every directory below it; see
    /// [`FileTree::expand_all`]. Does nothing on a file.
    pub fn expand_all_selected(&mut self) {
        if let Some((_, row)) = self.selection() {
            self.expand_all(&row.path);
        }
    }

    /// Collapse every directory except the root, which stays open (Zed's
    /// `CollapseAllEntries`). If the selected row disappears, the selection moves to the
    /// nearest ancestor that is still visible.
    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.expanded.insert(self.root.clone());
        self.sync();
    }

    /// Expand the directory at `path` (absolute, or relative to the root), reading it
    /// from disk if needed. Returns whether it is expanded afterwards.
    ///
    /// Ignored directories expand like any other. A directory that cannot be read, or a
    /// symbolic link that leads back to one of its own ancestors, stays collapsed. A
    /// directory inside a collapsed ancestor is remembered and shows as expanded once
    /// the ancestor opens.
    pub fn expand(&mut self, path: &Path) -> bool {
        let path = resolve(&self.root, path);
        if !path.starts_with(&self.root) {
            return false;
        }
        let is_dir = match self.rows.iter().find(|row| row.path == path) {
            Some(row) => row.is_dir(),
            None => fs::metadata(&path).is_ok_and(|meta| meta.is_dir()),
        };
        if !is_dir {
            return false;
        }
        self.expanded.insert(path.clone());
        self.sync();
        self.expanded.contains(&path)
    }

    /// Collapse the directory at `path`. Returns whether it was expanded. Directories
    /// open inside it are remembered and reappear when it is expanded again.
    pub fn collapse(&mut self, path: &Path) -> bool {
        let path = resolve(&self.root, path);
        let was_expanded = self.expanded.remove(&path);
        if was_expanded {
            self.sync();
        }
        was_expanded
    }

    /// Whether the directory at `path` is marked expanded (it may still be hidden inside
    /// a collapsed ancestor).
    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(&resolve(&self.root, path))
    }

    /// Every directory marked expanded, sorted. Together with [`FileTree::expand`] this
    /// is enough to save and restore the panel between sessions.
    pub fn expanded_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        paths.sort();
        paths
    }

    /// Expand the visible directory at `path` and every directory below it. Returns
    /// whether anything was expanded.
    ///
    /// Ignored directories below `path` are left closed unless `path` itself is ignored,
    /// and symbolic links to directories are not followed, so "expand all" on a project
    /// never wanders into `target/` or out of the project. At most
    /// [`EXPAND_ALL_LIMIT`] directories are opened per call.
    pub fn expand_all(&mut self, path: &Path) -> bool {
        let path = resolve(&self.root, path);
        let Some(start) = self.rows.iter().find(|row| row.path == path).cloned() else {
            return false;
        };
        if !start.is_dir() || !self.expand(&start.path) {
            return false;
        }
        let mut frontier = vec![start.path.clone()];
        let mut budget = EXPAND_ALL_LIMIT;
        while !frontier.is_empty() && budget > 0 {
            let mut next = Vec::new();
            for dir in frontier {
                let Some(listing) = self.listings.get(&dir) else {
                    continue;
                };
                let through_link = listing.real.is_some();
                let children: Vec<PathBuf> = listing
                    .entries
                    .iter()
                    .filter(|entry| entry.is_dir && !entry.symlink)
                    .filter(|entry| start.ignored || !entry.ignored)
                    .map(|entry| dir.join(&entry.name))
                    .collect();
                for child in children {
                    if budget == 0 {
                        break;
                    }
                    budget -= 1;
                    if !self.listings.contains_key(&child) {
                        match self.read_listing(&child, through_link) {
                            Some(listing) => self.listings.insert(child.clone(), listing),
                            None => continue,
                        };
                    }
                    self.expanded.insert(child.clone());
                    next.push(child);
                }
            }
            frontier = next;
        }
        self.sync();
        true
    }

    /// Make `path` (absolute, or relative to the root) visible by expanding the
    /// directories above it, select it, and return its row index.
    ///
    /// The directories need not be loaded yet. Returns `None`, changing nothing, when the
    /// path does not exist, is outside the root or is always hidden (inside `.git`, for
    /// instance).
    pub fn reveal(&mut self, path: &Path) -> Option<usize> {
        let path = resolve(&self.root, path);
        let relative = path.strip_prefix(&self.root).ok()?.to_path_buf();
        if has_hidden_component(&relative) {
            return None;
        }
        fs::symlink_metadata(&path).ok()?;
        let mut dir = self.root.clone();
        self.expanded.insert(dir.clone());
        if let Some(parent) = relative.parent() {
            for component in parent.components() {
                dir.push(component);
                self.expanded.insert(dir.clone());
            }
        }
        self.sync();
        let index = self.rows.iter().position(|row| row.path == path)?;
        self.selected = Some(index);
        Some(index)
    }

    /// Re-read the directories affected by a change to each of `paths` (absolute, or
    /// relative to the root): the directory each path is in and, if it is a visible
    /// directory, the path itself. Directories that are not visible cost nothing.
    ///
    /// Expansion state is kept. The selection follows its path; if that path is gone, the
    /// nearest surviving row is selected (the next sibling, else the previous one, else
    /// the parent). A change to a `.gitignore`, `.ignore` or `.git/info/exclude` reloads
    /// the ignore rules and re-reads everything visible.
    pub fn refresh<P: AsRef<Path>>(&mut self, paths: &[P]) {
        let mut rules_changed = false;
        for path in paths {
            let path = resolve(&self.root, path.as_ref());
            if !path.starts_with(&self.root) {
                continue;
            }
            rules_changed |= is_ignore_file(&path);
            if let Some(parent) = path.parent() {
                self.listings.remove(parent);
            }
            self.listings.remove(&path);
        }
        if rules_changed {
            self.rules.reload();
            self.listings.clear();
        }
        self.sync();
    }

    /// Reload the ignore rules and re-read every visible directory, keeping expansion
    /// state and the selection as [`FileTree::refresh`] does. Use it when the watcher
    /// reports a rescan.
    pub fn refresh_all(&mut self) {
        self.root_real = fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        self.rules.reload();
        self.listings.clear();
        self.sync();
    }

    /// The directory a "new file" or "paste" should go to: the selected directory, the
    /// directory of the selected file, or the root when nothing is selected.
    pub fn creation_dir(&self) -> PathBuf {
        match self.selected_row() {
            Some(row) if row.is_dir() => row.path.clone(),
            Some(row) => row
                .path
                .parent()
                .map_or_else(|| self.root.clone(), Path::to_path_buf),
            None => self.root.clone(),
        }
    }

    /// Create an empty file in `dir`, reveal and select it, and return its path.
    ///
    /// `name` may contain `/` to create the directories in between, and a name ending in
    /// `/` creates a directory, as in Zed. See [`fileops::create_file`] for the errors.
    pub fn create_file(&mut self, dir: &Path, name: &str) -> Result<PathBuf, FileOpError> {
        let dir = self.inside_root(dir)?;
        let created = fileops::create_file(&dir, name)?;
        self.show_new_entry(&dir, &created);
        Ok(created)
    }

    /// Create a directory in `dir`, reveal and select it, and return its path. `name`
    /// may contain `/` to create nested directories.
    pub fn create_dir(&mut self, dir: &Path, name: &str) -> Result<PathBuf, FileOpError> {
        let dir = self.inside_root(dir)?;
        let created = fileops::create_dir(&dir, name)?;
        self.show_new_entry(&dir, &created);
        Ok(created)
    }

    /// Rename the entry at `path`, select it under its new name, and return the new path.
    ///
    /// Refuses to overwrite an existing entry. `new_name` may contain `/` to move the
    /// entry into a subdirectory. A renamed directory stays expanded, as does everything
    /// open inside it. See [`fileops::rename`].
    pub fn rename(&mut self, path: &Path, new_name: &str) -> Result<PathBuf, FileOpError> {
        let path = self.entry_path(path)?;
        let renamed = fileops::rename(&path, new_name)?;
        if renamed.starts_with(&self.root) {
            self.show_moved_entry(&path, &renamed);
        } else {
            self.forget_entry(&path);
        }
        Ok(renamed)
    }

    /// Copy the entry at `path` next to itself as `name copy`, `name copy 2`, ...,
    /// select the copy and return its path. See [`fileops::duplicate`].
    pub fn duplicate(&mut self, path: &Path) -> Result<PathBuf, FileOpError> {
        let path = self.entry_path(path)?;
        let copy = fileops::duplicate(&path)?;
        let dir = copy.parent().map(Path::to_path_buf).unwrap_or_default();
        self.show_new_entry(&dir, &copy);
        Ok(copy)
    }

    /// Move the entry at `path` to the system trash. The selection moves to the nearest
    /// surviving row.
    pub fn trash(&mut self, path: &Path) -> Result<(), FileOpError> {
        let path = self.entry_path(path)?;
        (self.trash)(&path)?;
        self.forget_entry(&path);
        Ok(())
    }

    /// Delete the entry at `path` for good, bypassing the trash. A directory goes with
    /// everything in it. The selection moves to the nearest surviving row.
    pub fn delete(&mut self, path: &Path) -> Result<(), FileOpError> {
        let path = self.entry_path(path)?;
        fileops::delete(&path)?;
        self.forget_entry(&path);
        Ok(())
    }

    /// Move the entry at `path` into the directory `dir`, select it there and return its
    /// new path. Refuses to overwrite and to move a directory into itself; see
    /// [`fileops::move_into`]. This is what dropping a dragged row on a directory does.
    pub fn move_into(&mut self, path: &Path, dir: &Path) -> Result<PathBuf, FileOpError> {
        let path = self.entry_path(path)?;
        let dir = self.inside_root(dir)?;
        let moved = fileops::move_into(&path, &dir)?;
        self.show_moved_entry(&path, &moved);
        Ok(moved)
    }

    /// Copy the entry at `path` into the directory `dir`, select the copy and return its
    /// path. A name clash yields `name copy`; see [`fileops::copy_into`].
    pub fn copy_into(&mut self, path: &Path, dir: &Path) -> Result<PathBuf, FileOpError> {
        let path = self.inside_root(path)?;
        let dir = self.inside_root(dir)?;
        let copy = fileops::copy_into(&path, &dir)?;
        self.show_new_entry(&dir, &copy);
        Ok(copy)
    }

    /// Put `paths` on the clipboard as cut: the next [`FileTree::paste`] moves them.
    /// Paths outside the root, the root itself and paths inside another listed path are
    /// dropped. An empty result clears the clipboard.
    pub fn cut<P: AsRef<Path>>(&mut self, paths: &[P]) {
        self.set_clipboard(paths, true);
    }

    /// Put `paths` on the clipboard as copied: the next [`FileTree::paste`] copies them,
    /// and they stay on the clipboard for further pastes. Filtered as in
    /// [`FileTree::cut`].
    pub fn copy<P: AsRef<Path>>(&mut self, paths: &[P]) {
        self.set_clipboard(paths, false);
    }

    /// Cut the selected entry. Returns `false` (leaving the clipboard alone) when nothing
    /// is selected or the selection is the root.
    pub fn cut_selected(&mut self) -> bool {
        self.clip_selected(true)
    }

    /// Copy the selected entry. Returns `false` (leaving the clipboard alone) when
    /// nothing is selected or the selection is the root.
    pub fn copy_selected(&mut self) -> bool {
        self.clip_selected(false)
    }

    /// What is on the clipboard, if anything.
    pub fn clipboard(&self) -> Option<&Clipboard> {
        self.clipboard.as_ref()
    }

    /// Empty the clipboard.
    pub fn clear_clipboard(&mut self) {
        self.clipboard = None;
    }

    /// Paste the clipboard into `target`: into it if it is a directory, next to it if it
    /// is a file. Returns the new paths, selecting the first.
    ///
    /// Cut entries are moved and the clipboard is emptied; copied entries are copied and
    /// stay on the clipboard. An entry whose name is taken in the target directory gets
    /// `name copy` (a cut entry pasted where it already is stays put). Nothing is
    /// overwritten. An empty clipboard pastes nothing. If an entry fails, the ones before
    /// it stay pasted, cut entries not yet moved stay on the clipboard, and the error is
    /// returned.
    pub fn paste(&mut self, target: &Path) -> Result<Vec<PathBuf>, FileOpError> {
        let target = self.inside_root(target)?;
        let dir = match fs::metadata(&target) {
            Ok(meta) if meta.is_dir() => target,
            Ok(_) => target
                .parent()
                .map_or_else(|| self.root.clone(), Path::to_path_buf),
            Err(_) => return Err(FileOpError::NotFound(target)),
        };
        let Some(clipboard) = self.clipboard.clone() else {
            return Ok(Vec::new());
        };

        let mut pasted = Vec::new();
        let mut failure = None;
        let mut remaining = clipboard.paths.clone();
        for path in &clipboard.paths {
            let result = if clipboard.cut {
                fileops::move_into_unique(path, &dir)
            } else {
                fileops::copy_into(path, &dir)
            };
            match result {
                Ok(new_path) => {
                    if clipboard.cut {
                        self.carry_expansion(path, &new_path);
                        self.invalidate_parent(path);
                        remaining.retain(|left| left != path);
                    }
                    pasted.push(new_path);
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if clipboard.cut {
            self.clipboard = (!remaining.is_empty()).then_some(Clipboard {
                paths: remaining,
                cut: true,
            });
        }

        self.listings.remove(&dir);
        match pasted.first() {
            Some(first) if self.reveal(first).is_some() => {}
            _ => self.sync(),
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(pasted),
        }
    }

    /// Paste the clipboard at the selection: see [`FileTree::paste`] and
    /// [`FileTree::creation_dir`].
    pub fn paste_at_selection(&mut self) -> Result<Vec<PathBuf>, FileOpError> {
        let dir = self.creation_dir();
        self.paste(&dir)
    }

    /// Replace what [`FileTree::trash`] does with the path it is given. The default
    /// moves it to the system trash ([`fileops::trash`]); tests and apps that want a
    /// different policy (their own trash folder, a confirmation, a permanent delete)
    /// install their own.
    pub fn set_trash_handler(
        &mut self,
        handler: impl Fn(&Path) -> Result<(), FileOpError> + Send + 'static,
    ) {
        self.trash = Box::new(handler);
    }

    fn selection(&self) -> Option<(usize, Row)> {
        let index = self.selected?;
        Some((index, self.rows.get(index)?.clone()))
    }

    fn parent_index(&self, index: usize) -> Option<usize> {
        let depth = self.rows.get(index)?.depth;
        if depth == 0 {
            return None;
        }
        self.rows[..index]
            .iter()
            .rposition(|row| row.depth == depth - 1)
    }

    /// `path` made absolute, or an error if it is not inside the root.
    fn inside_root(&self, path: &Path) -> Result<PathBuf, FileOpError> {
        let path = resolve(&self.root, path);
        if path.starts_with(&self.root) {
            Ok(path)
        } else {
            Err(FileOpError::OutsideRoot(path))
        }
    }

    /// `path` made absolute, or an error if it is the root or not inside it.
    fn entry_path(&self, path: &Path) -> Result<PathBuf, FileOpError> {
        let path = self.inside_root(path)?;
        if path == self.root {
            Err(FileOpError::IsRoot)
        } else {
            Ok(path)
        }
    }

    fn set_clipboard<P: AsRef<Path>>(&mut self, paths: &[P], cut: bool) {
        let mut kept: Vec<PathBuf> = Vec::new();
        for path in paths {
            let Ok(path) = self.entry_path(path.as_ref()) else {
                continue;
            };
            if !kept.contains(&path) {
                kept.push(path);
            }
        }
        // An entry inside another clipboard entry travels with it.
        let all = kept.clone();
        kept.retain(|path| {
            !all.iter()
                .any(|other| other != path && path.starts_with(other))
        });
        self.clipboard = (!kept.is_empty()).then_some(Clipboard { paths: kept, cut });
    }

    fn clip_selected(&mut self, cut: bool) -> bool {
        let Some(path) = self.selected_path().map(Path::to_path_buf) else {
            return false;
        };
        if path == self.root {
            return false;
        }
        self.set_clipboard(&[path], cut);
        true
    }

    fn invalidate_parent(&mut self, path: &Path) {
        if let Some(parent) = path.parent() {
            self.listings.remove(parent);
        }
    }

    /// After something new appeared at `created`, somewhere below `dir`: re-read what
    /// changed, then reveal and select it.
    fn show_new_entry(&mut self, dir: &Path, created: &Path) {
        for ancestor in created.ancestors().skip(1) {
            self.listings.remove(ancestor);
            if ancestor == dir {
                break;
            }
        }
        if self.reveal(created).is_none() {
            self.sync();
        }
    }

    /// After the entry at `from` became `to`: carry its state over, re-read what changed,
    /// then reveal and select it.
    fn show_moved_entry(&mut self, from: &Path, to: &Path) {
        if from == to {
            return;
        }
        self.carry_expansion(from, to);
        if let Some(clipboard) = self.clipboard.as_mut() {
            for path in &mut clipboard.paths {
                if let Some(moved) = rebase(path, from, to) {
                    *path = moved;
                }
            }
        }
        self.invalidate_parent(from);
        for ancestor in to.ancestors().skip(1) {
            if !ancestor.starts_with(&self.root) {
                break;
            }
            self.listings.remove(ancestor);
        }
        if self.reveal(to).is_none() {
            self.sync();
        }
    }

    /// After the entry at `path` was removed: drop it from the clipboard and re-read its
    /// directory. The selection is repaired by `sync`.
    fn forget_entry(&mut self, path: &Path) {
        if let Some(clipboard) = self.clipboard.as_mut() {
            clipboard.paths.retain(|clipped| !clipped.starts_with(path));
            if clipboard.paths.is_empty() {
                self.clipboard = None;
            }
        }
        self.invalidate_parent(path);
        self.sync();
    }

    /// Move the expansion state of `from` and everything below it to `to`.
    fn carry_expansion(&mut self, from: &Path, to: &Path) {
        let moved: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|dir| dir.starts_with(from))
            .cloned()
            .collect();
        for dir in moved {
            self.expanded.remove(&dir);
            if let Some(new_dir) = rebase(&dir, from, to) {
                self.expanded.insert(new_dir);
            }
        }
        self.listings.retain(|dir, _| !dir.starts_with(from));
    }

    /// Read one directory from disk. `through_link` says the directory is reached through
    /// a symbolic link, in which case it is refused (`None`) when it leads back to one of
    /// its own ancestors. Unreadable directories are `None` too.
    fn read_listing(&mut self, dir: &Path, through_link: bool) -> Option<Listing> {
        let real = if through_link {
            let real = fs::canonicalize(dir).ok()?;
            if self.is_real_path_of_ancestor(dir, &real) {
                log::debug!("not expanding {}: it loops back", dir.display());
                return None;
            }
            Some(real)
        } else {
            None
        };

        let relative_dir = dir.strip_prefix(&self.root).ok()?.to_path_buf();
        let mut entries = Vec::new();
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let name = entry.file_name();
            if is_always_hidden(&name) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let symlink = file_type.is_symlink();
            let is_dir = if symlink {
                fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir())
            } else {
                file_type.is_dir()
            };
            let ignored = self.rules.is_ignored(&relative_dir.join(&name), is_dir);
            entries.push(Entry {
                display: name.to_string_lossy().into_owned(),
                name,
                is_dir,
                ignored,
                symlink,
            });
        }
        entries.sort_by(|a, b| {
            compare_entries(a.is_dir, &a.display, b.is_dir, &b.display)
                .then_with(|| a.name.cmp(&b.name))
        });
        Some(Listing { entries, real })
    }

    /// Whether `real` is the real location of a directory above `dir`, which would make
    /// expanding `dir` show the same directories again without end.
    fn is_real_path_of_ancestor(&self, dir: &Path, real: &Path) -> bool {
        dir.ancestors()
            .skip(1)
            .take_while(|ancestor| ancestor.starts_with(&self.root))
            .any(|ancestor| {
                match self
                    .listings
                    .get(ancestor)
                    .and_then(|listing| listing.real.as_deref())
                {
                    Some(ancestor_real) => ancestor_real == real,
                    // No link on the way down to this ancestor, so it sits at the
                    // same relative place under the real root.
                    None => match ancestor.strip_prefix(&self.root) {
                        Ok(rest) if rest.as_os_str().is_empty() => self.root_real == real,
                        Ok(rest) => self.root_real.join(rest) == real,
                        Err(_) => false,
                    },
                }
            })
    }

    /// Bring everything in line with `expanded`: load the directories that became
    /// visible, drop the ones that no longer are, forget state for directories that
    /// vanished, rebuild the rows and put the selection back.
    fn sync(&mut self) {
        let previous = self
            .selected
            .and_then(|index| self.rows.get(index).cloned());

        let mut visible = HashSet::new();
        let mut pending = vec![(self.root.clone(), false)];
        while let Some((dir, through_link)) = pending.pop() {
            if !self.expanded.contains(&dir) {
                continue;
            }
            if !self.listings.contains_key(&dir) {
                match self.read_listing(&dir, through_link) {
                    Some(listing) => {
                        self.listings.insert(dir.clone(), listing);
                    }
                    None if dir == self.root => {
                        self.listings.insert(dir.clone(), Listing::default());
                    }
                    None => {
                        self.expanded.remove(&dir);
                        continue;
                    }
                }
            }
            let listing = &self.listings[&dir];
            let inherited = listing.real.is_some();
            for entry in &listing.entries {
                if entry.is_dir {
                    let child = dir.join(&entry.name);
                    if self.expanded.contains(&child) {
                        pending.push((child, inherited || entry.symlink));
                    }
                }
            }
            visible.insert(dir);
        }
        self.listings.retain(|dir, _| visible.contains(dir));
        self.forget_vanished();

        let mut rows = Vec::with_capacity(self.rows.len().max(16));
        rows.push(Row {
            name: self.root.file_name().map_or_else(
                || self.root.to_string_lossy().into_owned(),
                |name| name.to_string_lossy().into_owned(),
            ),
            path: self.root.clone(),
            depth: 0,
            kind: RowKind::Dir {
                expanded: self.expanded.contains(&self.root),
            },
            ignored: false,
            symlink: false,
            is_root: true,
        });
        if self.expanded.contains(&self.root) {
            self.push_children(&self.root, 1, &mut rows);
        }
        self.rows = rows;

        self.selected = previous.and_then(|old| {
            self.rows
                .iter()
                .position(|row| row.path == old.path)
                .or_else(|| self.nearest_surviving(&old))
        });
    }

    fn push_children(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        let Some(listing) = self.listings.get(dir) else {
            return;
        };
        for entry in &listing.entries {
            let path = dir.join(&entry.name);
            let expanded =
                entry.is_dir && self.expanded.contains(&path) && self.listings.contains_key(&path);
            rows.push(Row {
                name: entry.display.clone(),
                depth,
                kind: if entry.is_dir {
                    RowKind::Dir { expanded }
                } else {
                    RowKind::File
                },
                ignored: entry.ignored,
                symlink: entry.symlink,
                is_root: false,
                path: path.clone(),
            });
            if expanded {
                self.push_children(&path, depth + 1, rows);
            }
        }
    }

    /// Drop the expansion state of directories that a visible parent no longer lists,
    /// together with everything remembered below them.
    fn forget_vanished(&mut self) {
        let gone: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|dir| **dir != self.root)
            .filter(|dir| {
                let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
                    return false;
                };
                self.listings.get(parent).is_some_and(|listing| {
                    !listing
                        .entries
                        .iter()
                        .any(|entry| entry.is_dir && entry.name == name)
                })
            })
            .cloned()
            .collect();
        if !gone.is_empty() {
            self.expanded
                .retain(|dir| !gone.iter().any(|lost| dir.starts_with(lost)));
        }
    }

    /// The row to select when the row `old` is no longer there: the entry that now
    /// stands where it stood (its next sibling), else the previous sibling, else the
    /// parent; if the parent is gone as well, the same one level up.
    fn nearest_surviving(&self, old: &Row) -> Option<usize> {
        let mut lost = old.path.clone();
        let mut lost_name = old.name.clone();
        let mut lost_is_dir = old.is_dir();
        loop {
            let parent = lost.parent()?.to_path_buf();
            if !parent.starts_with(&self.root) {
                return Some(0);
            }
            if let Some(parent_index) = self.rows.iter().position(|row| row.path == parent) {
                let parent_depth = self.rows[parent_index].depth;
                let mut last_child = None;
                for (index, row) in self.rows.iter().enumerate().skip(parent_index + 1) {
                    if row.depth <= parent_depth {
                        break;
                    }
                    if row.depth != parent_depth + 1 {
                        continue;
                    }
                    let after_lost =
                        compare_entries(row.is_dir(), &row.name, lost_is_dir, &lost_name)
                            == Ordering::Greater;
                    if after_lost {
                        return Some(index);
                    }
                    last_child = Some(index);
                }
                return Some(last_child.unwrap_or(parent_index));
            }
            lost_name = parent
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            lost_is_dir = true;
            lost = parent;
        }
    }
}

/// `path` with its leading `from` replaced by `to`, or `None` if it is not under `from`.
fn rebase(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebase_replaces_the_prefix() {
        let (from, to) = (Path::new("/p/old"), Path::new("/p/new"));
        assert_eq!(
            rebase(Path::new("/p/old"), from, to),
            Some(PathBuf::from("/p/new"))
        );
        assert_eq!(
            rebase(Path::new("/p/old/a/b"), from, to),
            Some(PathBuf::from("/p/new/a/b"))
        );
        assert_eq!(rebase(Path::new("/p/older"), from, to), None);
        assert_eq!(rebase(Path::new("/p"), from, to), None);
    }
}
