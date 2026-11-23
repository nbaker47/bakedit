//! Embedded terminal: pty, VT emulation, key and mouse encoding, link detection.

#![cfg(unix)]
#![warn(missing_docs)]

pub mod keys;

pub use keys::{encode_key, KeyModes};
