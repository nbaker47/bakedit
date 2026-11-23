//! Languages and syntax highlighting for bakedit.
//!
//! Two things live here:
//!
//! * The **language registry**: what a file is ([`detect`], [`language_for_path`]) and the
//!   editing conventions that follow from it ([`Language`]): comment markers, brackets,
//!   indentation, soft wrap.
//! * **Highlighting** with tree-sitter: a [`Highlighter`] turns text into
//!   [`HighlightSpan`]s, each naming a [`CaptureId`] that a theme colours. Languages embedded
//!   in other languages (code in a Markdown fence, a script in HTML) are highlighted as
//!   themselves. [`LineHighlights`] files the spans by line for drawing,
//!   [`bracket_marks`] works out bracket nesting for rainbow brackets, and [`outline`] lists
//!   a file's symbols.
//!
//! ```
//! use bakedit_syntax::{capture_name, detect, Highlighter};
//! use std::path::Path;
//!
//! let language = detect(Path::new("main.rs"), None);
//! let text = "fn main() {}";
//! let spans = Highlighter::new().highlight(language, text, None).unwrap();
//! assert_eq!(&text[spans[0].range()], "fn");
//! assert_eq!(capture_name(spans[0].capture), "keyword");
//! ```
//!
//! # Offsets and lines
//!
//! Everything here is in **byte** offsets into the UTF-8 text, not the `char` offsets the
//! rope uses; convert at the boundary. Line numbers are 0-based and count the line breaks a
//! [`LineBreaks`] names, which by default are the ones a `ropey` rope counts.
//!
//! # Threads
//!
//! Languages, capture ids and loaded grammars are global, immutable and shared. A
//! [`Highlighter`] is not shared: keep one per thread. Grammars load lazily, so the first
//! text of a language costs a few milliseconds more than the rest.

mod brackets;
mod capture;
mod error;
mod grammar;
mod highlight;
mod language;
mod lines;
mod lua_pattern;
mod outline;
mod registry;

pub use brackets::{bracket_marks, BracketMark};
pub use capture::{capture_count, capture_id, capture_name, CaptureId, StyleCache};
pub use error::SyntaxError;
pub use highlight::{spans_in, HighlightSpan, Highlighter};
pub use language::{
    detect, language_by_id, language_by_name, language_for_path, languages, plain_text, Language,
    Pair,
};
pub use lines::{LineBreaks, LineHighlights};
pub use outline::{outline, Symbol, SymbolKind};
