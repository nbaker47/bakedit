//! Embedded terminal: pty, VT emulation, key and mouse encoding, link detection.

#![cfg(unix)]
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod event;
pub mod input;
pub mod keys;
pub mod links;

pub use config::{
    find_program, inherited_env_names, split_command_line, TermColors, TermConfig, DEFAULT_COMMAND,
};
pub use error::{Result, TermError};
pub use event::TermEvent;
pub use input::{encode_alt_scroll, encode_focus, encode_mouse, encode_paste, MouseModes};
pub use keys::{encode_key, KeyModes};
pub use links::{
    default_search_bases, detect_links, link_at, resolve_path, resolve_reference, LinkSpan,
    LinkTarget, ResolvedPath,
};
