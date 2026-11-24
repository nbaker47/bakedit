//! Shared vocabulary for every bakedit crate.
//!
//! Nothing in here knows about terminals, files or the UI. It only defines the types the
//! other crates exchange: key and mouse events as the host terminal reports them, named
//! actions as the keymap produces them, and colours/themes as the UI draws them.

pub mod action;
pub mod color;
pub mod event;
pub mod jsonc;
pub mod keys;
pub mod mouse;
pub mod theme;

pub use action::Action;
pub use color::{Color, Rgb};
pub use event::{ColorSlot, InputEvent, TerminalReply};
pub use keys::{us_shifted, us_unshifted, Key, KeyEvent, KeyKind, ModKey, Mods};
pub use mouse::{MouseButton, MouseEvent, MouseKind};
pub use theme::{SyntaxStyle, TerminalPalette, Theme, ThemeError, BUILTIN_THEME_NAMES};
