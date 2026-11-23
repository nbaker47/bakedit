//! Project model: file tree, file index and fuzzy finder, project search, file watching, icons.
//!
//! Everything about the project directory that is not git and not text. Nothing in here
//! draws: every type returns plain data and the app decides how it looks.
//!
//! | Module | What it gives the app |
//! |---|---|
//! | [`tree`] | [`FileTree`], the model behind the project panel: lazy directories, selection, file operations |
//! | [`index`] | [`FileIndex`], the background list of project files that the file finder searches |
//! | [`fuzzy`] | [`parse_query`] and the path-aware fuzzy matching behind [`FileIndex::search`] |
//! | [`mod@search`] | [`search()`] and [`replace_all`], project-wide text search and replace |
//! | [`watcher`] | [`Watcher`], debounced file-system events with git and ignore awareness |
//! | [`icons`] | [`icon_for`], Nerd Font glyphs in the Catppuccin Frappé colours |
//! | [`fileops`] | the file operations the tree performs, usable on their own |
//!
//! # Paths
//!
//! The project root is used exactly as the caller wrote it (made absolute, never
//! canonicalised), and every absolute path this crate hands back starts with that same
//! root. The tree, the watcher and the file operations speak absolute paths; the file index
//! and project search report paths relative to the root, because that is what a picker
//! shows. Both kinds are accepted wherever a path is taken as input.
//!
//! # What counts as hidden and what counts as ignored
//!
//! Names in [`ALWAYS_HIDDEN`] (`.git`, `.DS_Store` and friends) never appear anywhere.
//! Everything else is visible, dotfiles included. Paths matched by `.gitignore`, `.ignore`,
//! `.git/info/exclude` or the global gitignore are *ignored*: the tree still shows them
//! (flagged, so the UI can dim them), while the index, project search and the watcher
//! leave them out unless asked otherwise.

#![warn(missing_docs)]

pub mod fileops;
pub mod fuzzy;
pub mod icons;
pub mod index;
pub mod search;
pub mod tree;
pub mod watcher;

mod ignore_rules;
mod order;

pub use fileops::FileOpError;
pub use fuzzy::{parse_query, FileMatch, FileQuery};
pub use icons::{ascii_icon_for, icon_for, Icon};
pub use ignore_rules::{is_always_hidden, ALWAYS_HIDDEN};
pub use index::{FileIndex, IndexOptions, IndexSnapshot};
pub use search::{
    replace_all, search, search_with_wake, LineMatch, ReplaceOutcome, ReplaceResult, SearchError,
    SearchEvent, SearchHandle, SearchOptions, SkipReason,
};
pub use tree::{Clipboard, FileTree, Row, RowKind};
pub use watcher::{FsBatch, WatchError, Watcher};

#[cfg(test)]
mod tests {
    use super::*;

    fn is_send<T: Send>() {}
    fn is_send_and_sync<T: Send + Sync>() {}

    /// The threading guarantees the documentation promises, checked by the compiler.
    #[test]
    fn handles_can_be_moved_and_shared_between_threads() {
        // The tree is owned by one thread at a time (it can be moved, not shared).
        is_send::<FileTree>();
        // These may be called from any thread.
        is_send_and_sync::<FileIndex>();
        is_send_and_sync::<IndexSnapshot>();
        is_send_and_sync::<Watcher>();
        is_send_and_sync::<SearchHandle>();
        // And the data they hand out can go anywhere.
        is_send_and_sync::<Row>();
        is_send_and_sync::<FsBatch>();
        is_send_and_sync::<SearchEvent>();
        is_send_and_sync::<FileMatch>();
        is_send_and_sync::<SearchOptions>();
        is_send_and_sync::<FileOpError>();
        is_send_and_sync::<SearchError>();
        is_send_and_sync::<WatchError>();
    }
}
