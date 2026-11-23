//! bakedit, the application: everything behind the `bakedit` binary.

pub mod actions;
pub mod background;
pub mod chrome;
pub mod cli;
pub mod clipboard;
pub mod dump;
pub mod format;
pub mod fuzzy;
pub mod hints;
pub mod host;
pub mod ipc;
pub mod keys;
pub mod layout;
pub mod line_input;
pub mod minimap;
pub mod paint;
pub mod pane;
pub mod picker;
pub mod project;
pub mod session;
pub mod settings;
pub mod shell;
pub mod ui;
pub mod usage;
pub mod view;
pub mod workspace;

#[cfg(test)]
mod shell_tests;

#[cfg(test)]
mod keys_tests;
