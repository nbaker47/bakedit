//! Embedded terminal: pty, VT emulation, key and mouse encoding, link detection.

#![cfg(unix)]
#![warn(missing_docs)]

pub mod input;
pub mod keys;
pub mod links;

pub use input::{encode_alt_scroll, encode_focus, encode_mouse, encode_paste, MouseModes};
pub use keys::{encode_key, KeyModes};
pub use links::{
    default_search_bases, detect_links, link_at, resolve_path, resolve_reference, LinkSpan,
    LinkTarget, ResolvedPath,
};
