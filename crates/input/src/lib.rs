//! Host terminal input: escape sequence parser (kitty keyboard protocol, mouse, paste) and the built-in nav layer.
//!
//! Pure logic, no I/O. The app reads bytes from the host terminal and writes strings to it;
//! this crate says what the bytes mean and which strings to write:
//!
//! - [`Parser`] turns the bytes read from the host terminal into
//!   [`InputEvent`](bakedit_core::InputEvent)s.
//! - [`host`] holds every control string the app writes to the host terminal.
//! - [`NavLayer`] is the built-in nav layer: hold `;` and the home row becomes arrows and
//!   modifiers.
//! - [`HeldMods`] tracks which modifier keys are physically down, for mouse clicks.
//! - [`describe`] prints an event on one line for `--dump-input`.
//!
//! The order in the app's input loop is: bytes into the [`Parser`]; every key event it
//! produces into [`HeldMods::observe`]; then through [`NavLayer::process`]; and what comes
//! out of that is what the keymap sees.
//!
//! ```
//! use std::time::Instant;
//!
//! use bakedit_core::InputEvent;
//! use bakedit_input::{describe, HeldMods, NavLayer, NavLayerConfig, Parser};
//!
//! let mut parser = Parser::new();
//! let mut held = HeldMods::new();
//! let mut nav = NavLayer::new(NavLayerConfig::default());
//!
//! // `;` goes down and then `e`, as a terminal in kitty keyboard mode reports them.
//! let mut keys = Vec::new();
//! for event in parser.feed(b"\x1b[59;;59u\x1b[101;;101u") {
//!     if let InputEvent::Key(ev) = event {
//!         held.observe(&ev);
//!         keys.extend(nav.process(ev, Instant::now()));
//!     }
//! }
//! // The nav layer turned that into the Up arrow.
//! let lines: Vec<String> = keys.into_iter().map(|ev| describe(&InputEvent::Key(ev))).collect();
//! assert_eq!(lines, ["key press   up"]);
//! ```

#![warn(missing_docs)]

mod describe;
mod held;
pub mod host;
mod nav;
mod parser;

pub use describe::describe;
pub use held::HeldMods;
pub use nav::{NavLayer, NavLayerConfig};
pub use parser::{Parser, MAX_PASTE_LEN, MAX_STRING_LEN};
