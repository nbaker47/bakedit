//! Project model: file tree, file index and fuzzy finder, project search, file watching, icons.

pub mod icons;

mod ignore_rules;
mod order;

pub use icons::{ascii_icon_for, icon_for, Icon};
pub use ignore_rules::{is_always_hidden, ALWAYS_HIDDEN};
