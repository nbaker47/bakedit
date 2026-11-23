//! Text buffer and editing engine: rope, selections, motions, edits, undo, search.
//!
//! This crate is the headless half of the editor. It knows nothing about terminals, key
//! events or drawing: the app turns keys into [`Command`]s, applies them to an [`Editor`],
//! and draws what the editor reports.
//!
//! # The pieces
//!
//! - [`Buffer`]: the text in a rope, with line, column, byte and display-column
//!   conversions, atomic multi-range edits, unlimited undo, and the file it was loaded from
//!   (line endings, atomic save, change detection, minimal-diff reload).
//! - [`Selections`]: one or more cursors, always sorted and never overlapping.
//! - [`Editor`]: a buffer plus its selections plus [`EditSettings`]. [`Editor::apply`] runs a
//!   [`Command`] and returns an [`Outcome`]; [`Command::from_action`] maps Zed's action
//!   names and arguments onto commands.
//! - [`display`]: display widths and soft wrap, shared by the engine and the view so that
//!   they agree on where everything is.
//! - [`search`]: in-buffer find and replace.
//! - [`pairs`]: bracket and quote matching, including the "select between" behaviour.
//!
//! # Conventions
//!
//! Positions are `char` offsets into the text unless a name says otherwise (`byte`, `line`,
//! `col`, `display_col`). Lines are separated by `\n` only; the text in memory never
//! contains `\r`. Positions and ranges that are out of bounds are clamped, never a panic. A
//! cursor is never left inside a grapheme cluster.
//!
//! # Driving an editor
//!
//! ```
//! use bakedit_text::{Command, Editor};
//!
//! let mut editor = Editor::from_str("fn main() {}\n");
//! editor.set_cursor(11);
//!
//! // A key binding resolved to a Zed action name...
//! let newline = Command::from_action("editor::Newline", None).unwrap();
//! let outcome = editor.apply(newline);
//! assert!(outcome.text_changed());
//! assert_eq!(editor.buffer().to_string(), "fn main() {\n    \n}\n");
//!
//! // ...and typed text.
//! editor.apply(Command::Insert("run()".to_string()));
//! assert_eq!(editor.buffer().line(1), "    run()");
//!
//! editor.apply(Command::Undo);
//! editor.apply(Command::Undo);
//! assert_eq!(editor.buffer().to_string(), "fn main() {}\n");
//! ```

#![warn(missing_docs)]

pub mod buffer;
pub mod chars;
pub mod clock;
pub mod command;
mod diff;
pub mod display;
pub mod edit;
pub mod editor;
pub mod file;
pub mod graphemes;
pub mod history;
pub mod indent;
pub mod pairs;
pub mod search;
pub mod selection;

/// The rope library the buffer is built on, re-exported so the app can name `Rope` and
/// `RopeSlice` without depending on it separately.
pub use ropey;

pub use buffer::{Buffer, HistoryStep, Point};
pub use command::{Clipboard, ClipboardPiece, Command, Motion};
pub use edit::{Bias, BytePoint, Change, EditSummary, InputEdit};
pub use editor::{DisplayPosition, EditSettings, Editor, Outcome};
pub use file::{BufferError, DiskState, LineEnding, SaveOptions};
pub use history::UndoKind;
pub use indent::{detect_indent, IndentStyle, LineIndent};
pub use pairs::PairKind;
pub use search::{Search, SearchError, SearchQuery};
pub use selection::{Selection, Selections};
