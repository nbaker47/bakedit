//! Text buffer and editing engine: rope, selections, motions, edits, undo, search.

pub mod buffer;
pub mod chars;
pub mod clock;
mod diff;
pub mod display;
pub mod edit;
pub mod file;
pub mod graphemes;
pub mod history;
pub mod indent;
pub mod pairs;
pub mod search;
pub mod selection;

pub use ropey;

pub use buffer::{Buffer, HistoryStep, Point};
pub use edit::{Bias, BytePoint, Change, EditSummary, InputEdit};
pub use file::{BufferError, DiskState, LineEnding, SaveOptions};
pub use history::UndoKind;
pub use indent::{detect_indent, IndentStyle, LineIndent};
pub use pairs::PairKind;
pub use search::{Search, SearchError, SearchQuery};
pub use selection::{Selection, Selections};
