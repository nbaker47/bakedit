//! Project model: file tree, file index and fuzzy finder, project search, file watching, icons.

pub mod fileops;
pub mod fuzzy;
pub mod icons;
pub mod index;
pub mod tree;

mod ignore_rules;
mod order;

pub use fileops::FileOpError;
pub use fuzzy::{parse_query, FileMatch, FileQuery};
pub use icons::{ascii_icon_for, icon_for, Icon};
pub use ignore_rules::{is_always_hidden, ALWAYS_HIDDEN};
pub use index::{FileIndex, IndexOptions, IndexSnapshot};
pub use tree::{Clipboard, FileTree, Row, RowKind};
