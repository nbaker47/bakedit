//! Zed-compatible keymap engine: keystroke parsing, context predicates, binding resolution.
//!
//! The crate reads Zed's `keymap.json` format and resolves keys the way Zed's gpui does, so a
//! real Zed keymap works unchanged. It is pure logic: no I/O, no clock, no terminal.
//!
//! The pieces, in the order a key press meets them:
//!
//! - [`Keymap`] holds the bindings. [`Keymap::load`] parses a keymap file into it and returns
//!   a [`KeymapWarning`] for everything it had to skip. Load the built-in keymap first
//!   ([`KeymapSource::Default`]) and the user's after it ([`KeymapSource::User`]).
//! - [`KeyContext`] is what one element says about itself (`Editor`, `mode=full`). The app
//!   passes the stack of them from the workspace down to the focused element, and each
//!   keymap section chooses where it applies with a [`ContextPredicate`].
//! - [`Keystroke`] is one key as a keymap spells it, and knows whether a
//!   [`bakedit_core::KeyEvent`] types it.
//! - [`Matcher`] is the state machine for one window. Feed it every key event; it answers
//!   with [`Step`]s: bindings to try in order, or keys to treat as ordinary input. It holds
//!   keys back while they may still become a longer binding (`cmd-k cmd-s`).
//!
//! ```
//! use bakedit_core::{Key, KeyEvent, Mods};
//! use bakedit_keymap::{KeyContext, Keymap, KeymapSource, Matcher, Step};
//!
//! let mut keymap = Keymap::new();
//! let warnings = keymap.load(
//!     r#"[{ "context": "Editor", "bindings": { "cmd-k cmd-s": "zed::OpenKeymap" } }]"#,
//!     KeymapSource::User,
//! );
//! assert!(warnings.is_empty());
//!
//! let contexts = [KeyContext::new("Workspace"), KeyContext::new("Editor").set("mode", "full")];
//! let mut matcher = Matcher::new();
//! let cmd = |c| KeyEvent::new(Key::Char(c), Mods::CMD);
//!
//! let first = matcher.push(&keymap, &cmd('k'), &contexts, true);
//! assert!(first.pending && first.steps.is_empty());
//! assert_eq!(matcher.pending_display(), "cmd-k");
//!
//! let second = matcher.push(&keymap, &cmd('s'), &contexts, true);
//! let [Step::Dispatch { candidates, .. }] = second.steps.as_slice() else {
//!     panic!("a dispatch expected");
//! };
//! assert_eq!(candidates[0][0].name, "zed::OpenKeymap");
//! ```

#![warn(missing_docs)]

pub mod context;
pub mod keymap;
pub mod keystroke;
mod load;
pub mod matcher;

pub use context::{ContextError, ContextPredicate, KeyContext};
pub use keymap::{Binding, Keymap, KeymapSource};
pub use keystroke::{Keystroke, KeystrokeError};
pub use load::KeymapWarning;
pub use matcher::{Matcher, PendingTimer, PushResult, Step, PENDING_TIMEOUT};
