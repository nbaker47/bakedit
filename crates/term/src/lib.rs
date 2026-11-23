//! Embedded terminal: pty, VT emulation, key and mouse encoding, link detection.

#![cfg(unix)]
#![warn(missing_docs)]

pub mod input;
pub mod keys;

pub use input::{encode_alt_scroll, encode_focus, encode_mouse, encode_paste, MouseModes};
pub use keys::{encode_key, KeyModes};
