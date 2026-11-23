//! bakedit, the application: everything behind the `bakedit` binary.

pub mod actions;
pub mod app;
pub mod background;
pub mod chrome;
pub mod cli;
pub mod clipboard;
pub mod dump;
pub mod editor;
pub mod external;
pub mod factory;
pub mod format;
pub mod fuzzy;
pub mod hints;
pub mod host;
pub mod icons;
pub mod ipc;
pub mod items;
pub mod keys;
pub mod layout;
pub mod line_input;
pub mod logging;
pub mod minimap;
pub mod paint;
pub mod pane;
pub mod panels;
pub mod picker;
pub mod project;
pub mod session;
pub mod settings;
pub mod shell;
pub mod themes;
pub mod ui;
pub mod usage;
pub mod view;
pub mod window;
pub mod workspace;

#[cfg(test)]
mod shell_tests;

#[cfg(test)]
mod keys_tests;
