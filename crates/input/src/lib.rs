//! Host terminal input: escape sequence parser (kitty keyboard protocol, mouse, paste) and the built-in nav layer.
//!
//! Pure logic, no I/O. The app reads bytes from the host terminal and writes strings to it;
//! this crate says what the bytes mean and which strings to write:
//!
//! - [`Parser`] turns the bytes read from the host terminal into
//!   [`InputEvent`](bakedit_core::InputEvent)s.

#![warn(missing_docs)]

mod parser;

pub use parser::{Parser, MAX_PASTE_LEN, MAX_STRING_LEN};
