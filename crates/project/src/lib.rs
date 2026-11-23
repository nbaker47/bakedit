//! Project model: file tree, file index and fuzzy finder, project search, file watching, icons.

pub mod fileops;
pub mod icons;
pub mod tree;

mod ignore_rules;
mod order;

pub use fileops::FileOpError;
pub use icons::{ascii_icon_for, icon_for, Icon};
pub use ignore_rules::{is_always_hidden, ALWAYS_HIDDEN};
pub use tree::{Clipboard, FileTree, Row, RowKind};
