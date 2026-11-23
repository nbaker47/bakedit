//! Zed-compatible keymap engine: keystroke parsing, context predicates, binding resolution.
//!
//! The crate reads Zed's `keymap.json` format and resolves keys the way Zed's gpui does, so a
//! real Zed keymap works unchanged. It is pure logic: no I/O, no clock, no terminal.

#![warn(missing_docs)]

pub mod context;
pub mod keymap;
pub mod keystroke;
mod load;

pub use context::{ContextError, ContextPredicate, KeyContext};
pub use keymap::{Binding, Keymap, KeymapSource};
pub use keystroke::{Keystroke, KeystrokeError};
pub use load::KeymapWarning;
