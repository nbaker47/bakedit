//! The commands an editor understands, and how Zed's action names map onto them.

use serde_json::Value;

use crate::pairs::PairKind;

/// A way of moving a cursor. Used with [`Command::Move`] (the cursor goes there and nothing
/// stays selected) and [`Command::Select`] (the selection is extended to there).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Motion {
    /// One grapheme cluster back. Moving (not selecting) with something selected goes to
    /// the start of the selection instead.
    Left,
    /// One grapheme cluster forward. Moving with something selected goes to the end of the
    /// selection instead.
    Right,
    /// One display row up, keeping the goal column. On the first row, go to the start of
    /// the text.
    Up,
    /// One display row down, keeping the goal column. On the last row, go to the end of
    /// the text.
    Down,
    /// To the start of the current or previous word.
    PreviousWordStart,
    /// To the end of the current or next word.
    NextWordEnd,
    /// To the start of the next word (vim's `w`).
    NextWordStart,
    /// Like `PreviousWordStart`, also stopping inside `camelCase` and `snake_case`.
    PreviousSubwordStart,
    /// Like `NextWordEnd`, also stopping inside `camelCase` and `snake_case`.
    NextSubwordEnd,
    /// To the start of the line.
    BeginningOfLine {
        /// With soft wrap, stop at the start of the wrapped row first.
        stop_at_soft_wraps: bool,
        /// Go to the first non-blank character first; from there, or from inside the
        /// indentation, go to column 0; from column 0 go back to the first non-blank.
        stop_at_indent: bool,
    },
    /// To the end of the line.
    EndOfLine {
        /// With soft wrap, stop at the end of the wrapped row first.
        stop_at_soft_wraps: bool,
    },
    /// To the first non-blank character of the line (vim's `^`).
    FirstNonBlank,
    /// To the start of the text.
    Beginning,
    /// To the end of the text.
    End,
    /// To the blank line above the current paragraph, or the start of the text.
    StartOfParagraph,
    /// To the blank line below the current paragraph, or the end of the text.
    EndOfParagraph,
    /// Up by a page.
    PageUp {
        /// How many display rows a page is. 0 means [`crate::EditSettings::page_rows`].
        rows: usize,
    },
    /// Down by a page.
    PageDown {
        /// How many display rows a page is. 0 means [`crate::EditSettings::page_rows`].
        rows: usize,
    },
    /// Up by a number of display rows. 0 does nothing.
    UpByLines {
        /// How many rows.
        lines: usize,
    },
    /// Down by a number of display rows. 0 does nothing.
    DownByLines {
        /// How many rows.
        lines: usize,
    },
    /// Between the two brackets of the pair around the cursor: from next to one bracket to
    /// next to its partner, or from inside a pair to just before its closing bracket.
    EnclosingBracket,
    /// To a line and column, both 1-based and clamped to the text.
    GoToLine {
        /// 1-based line number.
        line: usize,
        /// 1-based char column.
        column: usize,
    },
}

/// One piece of a clipboard: the text of one selection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipboardPiece {
    /// The copied text.
    pub text: String,
    /// The piece is a whole line that was copied with nothing selected. It ends in `\n`.
    pub linewise: bool,
}

/// What `Copy` and `Cut` produce and `Paste` takes.
///
/// The system clipboard only holds text, so the app should keep the last `Clipboard` it put
/// there. When pasting, if the system clipboard still holds exactly [`Clipboard::text`],
/// paste the remembered value (so line-wise and per-cursor pasting work); otherwise paste
/// [`Clipboard::plain`] of whatever text is there.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Clipboard {
    /// The whole text, as it goes on the system clipboard: the pieces joined by line
    /// breaks.
    pub text: String,
    /// Every piece is a whole line copied with nothing selected. Pasting such a clipboard
    /// at a cursor inserts it above the cursor's line.
    pub linewise: bool,
    /// One entry per selection the text was taken from, in order. When pasting onto exactly
    /// this many selections, each selection receives its own piece. Empty for plain text.
    pub pieces: Vec<ClipboardPiece>,
}

impl Clipboard {
    /// A clipboard holding plain text from somewhere else.
    pub fn plain(text: impl Into<String>) -> Clipboard {
        Clipboard {
            text: text.into(),
            linewise: false,
            pieces: Vec::new(),
        }
    }

    /// A clipboard made of the text of several selections.
    pub fn from_pieces(pieces: Vec<ClipboardPiece>) -> Clipboard {
        let mut text = String::new();
        for (index, piece) in pieces.iter().enumerate() {
            if index > 0 && !pieces[index - 1].linewise {
                text.push('\n');
            }
            text.push_str(&piece.text);
        }
        let linewise = !pieces.is_empty() && pieces.iter().all(|piece| piece.linewise);
        Clipboard {
            text,
            linewise,
            pieces,
        }
    }

    /// True when there is nothing to paste.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Everything [`crate::Editor::apply`] can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Move every cursor by a motion, dropping the selections.
    Move(Motion),
    /// Extend every selection by a motion: the head moves, the anchor stays.
    Select(Motion),
    /// Select the whole text.
    SelectAll,
    /// Extend every selection to whole lines, line break included. Used again, it takes in
    /// the next line.
    SelectLine,
    /// With nothing selected, select the word under each cursor. Otherwise add the next
    /// occurrence of the selected text, wrapping around. Occurrences must be whole words
    /// when the first use started from a bare cursor.
    SelectNext {
        /// Move the newest selection to the next occurrence instead of adding one.
        replace_newest: bool,
    },
    /// Like `SelectNext`, searching backwards.
    SelectPrevious {
        /// Move the newest selection to the previous occurrence instead of adding one.
        replace_newest: bool,
    },
    /// Select every occurrence of the selected text (or of the word under the cursor).
    SelectAllMatches,
    /// Add a cursor in the same columns on the row above, skipping rows that are too
    /// short. Right after `AddSelectionBelow` it removes the last added cursor instead.
    AddSelectionAbove {
        /// Count buffer lines instead of soft-wrapped rows.
        skip_soft_wrap: bool,
    },
    /// Add a cursor in the same columns on the row below, skipping rows that are too
    /// short. Right after `AddSelectionAbove` it removes the last added cursor instead.
    AddSelectionBelow {
        /// Count buffer lines instead of soft-wrapped rows.
        skip_soft_wrap: bool,
    },
    /// Go back to the selections as they were before the last change to them.
    UndoSelection,
    /// Undo an `UndoSelection`.
    RedoSelection,
    /// With several selections, keep only the oldest. With one selection, collapse it to
    /// its cursor.
    Cancel,
    /// Grow every selection to the next larger unit: the word, the inside of the nearest
    /// brackets or quotes, those including the delimiters, the line, and so on outwards to
    /// the whole text.
    SelectLargerSyntaxNode,
    /// Step back through what `SelectLargerSyntaxNode` did.
    SelectSmallerSyntaxNode,
    /// Select the text between the nearest pair of delimiters of a kind, grow to the next
    /// pair when that is already selected, or jump into the first pair of the text.
    SelectBetween(PairKind),
    /// Type text: it replaces every selection. Brackets and quotes are closed, stepped
    /// over and wrapped around selections according to the settings.
    Insert(String),
    /// Break the line at every cursor, keeping the indentation.
    Newline,
    /// Open a new line above the cursor's line and move there.
    NewlineAbove,
    /// Open a new line below the cursor's line and move there.
    NewlineBelow,
    /// Delete the selection, or the grapheme cluster (or auto-closed pair, or indent step)
    /// before the cursor.
    Backspace,
    /// Delete the selection, or the grapheme cluster after the cursor.
    Delete,
    /// Delete back to the start of the word.
    DeleteToPreviousWordStart {
        /// Let the deletion cross line breaks instead of stopping at each one.
        ignore_newlines: bool,
        /// Let the deletion run past brackets instead of stopping at each one.
        ignore_brackets: bool,
    },
    /// Delete forward to the end of the word.
    DeleteToNextWordEnd {
        /// Let the deletion cross line breaks instead of stopping at each one.
        ignore_newlines: bool,
        /// Let the deletion run past brackets instead of stopping at each one.
        ignore_brackets: bool,
    },
    /// Delete back to the start of the sub-word.
    DeleteToPreviousSubwordStart {
        /// Let the deletion cross line breaks instead of stopping at each one.
        ignore_newlines: bool,
        /// Let the deletion run past brackets instead of stopping at each one.
        ignore_brackets: bool,
    },
    /// Delete forward to the end of the sub-word.
    DeleteToNextSubwordEnd {
        /// Let the deletion cross line breaks instead of stopping at each one.
        ignore_newlines: bool,
        /// Let the deletion run past brackets instead of stopping at each one.
        ignore_brackets: bool,
    },
    /// Delete from the cursor back to the start of the line; at the start of a line, the
    /// line break before it.
    DeleteToBeginningOfLine {
        /// Stop at the first non-blank character first (see [`Motion::BeginningOfLine`]).
        stop_at_indent: bool,
    },
    /// Delete from the cursor to the end of the line; at the end of a line, the line break.
    DeleteToEndOfLine,
    /// Delete the lines the selections touch.
    DeleteLine,
    /// Indent the lines of a selection, or insert indentation up to the next tab stop at a
    /// cursor.
    Tab,
    /// Remove one level of indentation from the lines the selections touch.
    Backtab,
    /// Add one level of indentation to the lines the selections touch.
    Indent,
    /// Remove one level of indentation from the lines the selections touch.
    Outdent,
    /// Move the lines the selections touch up by one line, selections included.
    MoveLineUp,
    /// Move the lines the selections touch down by one line, selections included.
    MoveLineDown,
    /// Duplicate the lines the selections touch, leaving the selections on the upper copy.
    DuplicateLineUp,
    /// Duplicate the lines the selections touch, moving the selections to the lower copy.
    DuplicateLineDown,
    /// Join the cursor's line with the next one, or all the lines of a selection, with a
    /// single space.
    JoinLines,
    /// Swap the characters on either side of the cursor and step past them.
    Transpose,
    /// Comment the lines the selections touch with a line-comment prefix, or uncomment
    /// them when every non-blank line is already commented.
    ToggleComments {
        /// The language's line-comment marker, such as `//` or `#`. A trailing space is
        /// optional. With an empty prefix the command does nothing.
        prefix: String,
        /// When commenting several lines, also put the marker on the blank ones.
        comment_empty_lines: bool,
        /// Afterwards move a bare cursor down one line.
        advance_downwards: bool,
    },
    /// Wrap every selection in block-comment delimiters, or remove them when they are
    /// already there. A bare cursor toggles the text of its line.
    ToggleBlockComments {
        /// The opening delimiter, such as `/*`. With an empty one the command does nothing.
        start: String,
        /// The closing delimiter, such as `*/`. With an empty one the command does nothing.
        end: String,
    },
    /// Copy the selections (or the whole line at a bare cursor) and delete them.
    Cut,
    /// Copy the selections, or the whole line at a bare cursor.
    Copy,
    /// Insert a clipboard at every selection.
    Paste(Clipboard),
    /// Cut from the cursor to the end of the line (or the line break when already there)
    /// into the editor's own kill ring.
    KillRingCut,
    /// Insert what `KillRingCut` took last.
    KillRingYank,
    /// Undo the last text change and restore the selections from before it.
    Undo,
    /// Redo the last undone text change.
    Redo,
}

fn bool_arg(args: Option<&Value>, key: &str, default: bool) -> bool {
    args.and_then(|value| value.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn count_arg(args: Option<&Value>, key: &str) -> usize {
    let value = match args {
        Some(Value::Object(map)) => map.get(key),
        // Zed also accepts a bare number where an action has a single numeric argument.
        Some(number @ Value::Number(_)) => Some(number),
        _ => None,
    };
    value
        .and_then(Value::as_u64)
        .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX))
}

fn str_arg<'a>(args: Option<&'a Value>, key: &str) -> &'a str {
    match args {
        Some(Value::Object(map)) => map.get(key).and_then(Value::as_str).unwrap_or(""),
        Some(Value::String(text)) => text,
        _ => "",
    }
}

/// The motion behind `editor::Move<suffix>` and `editor::Select<suffix>`. `moving` is true
/// for the `Move` form; the two forms have different defaults for soft-wrap stops.
fn motion_for(suffix: &str, args: Option<&Value>, moving: bool) -> Option<Motion> {
    Some(match suffix {
        "Left" => Motion::Left,
        "Right" => Motion::Right,
        "Up" => Motion::Up,
        "Down" => Motion::Down,
        "ToPreviousWordStart" => Motion::PreviousWordStart,
        "ToNextWordEnd" => Motion::NextWordEnd,
        "ToNextWordStart" => Motion::NextWordStart,
        "ToPreviousSubwordStart" => Motion::PreviousSubwordStart,
        "ToNextSubwordEnd" => Motion::NextSubwordEnd,
        "ToBeginningOfLine" => Motion::BeginningOfLine {
            stop_at_soft_wraps: bool_arg(args, "stop_at_soft_wraps", moving),
            stop_at_indent: bool_arg(args, "stop_at_indent", false),
        },
        "ToEndOfLine" => Motion::EndOfLine {
            stop_at_soft_wraps: bool_arg(args, "stop_at_soft_wraps", moving),
        },
        "ToFirstNonBlank" => Motion::FirstNonBlank,
        "ToBeginning" => Motion::Beginning,
        "ToEnd" => Motion::End,
        "ToStartOfParagraph" => Motion::StartOfParagraph,
        "ToEndOfParagraph" => Motion::EndOfParagraph,
        "PageUp" => Motion::PageUp {
            rows: count_arg(args, "rows"),
        },
        "PageDown" => Motion::PageDown {
            rows: count_arg(args, "rows"),
        },
        "UpByLines" => Motion::UpByLines {
            lines: count_arg(args, "lines"),
        },
        "DownByLines" => Motion::DownByLines {
            lines: count_arg(args, "lines"),
        },
        "ToEnclosingBracket" => Motion::EnclosingBracket,
        _ => return None,
    })
}

impl Command {
    /// The command for a Zed action name and its arguments, as they appear in a keymap.
    ///
    /// Covers the `editor::` actions this engine implements, with Zed's argument names and
    /// defaults, plus four of bakedit's own: `editor::MoveToNextWordStart`,
    /// `editor::MoveToFirstNonBlank`, `editor::GoToLine` (`{"line", "column"}`, 1-based)
    /// and `editor::SelectBetween` (`{"kind": "paren" | "bracket" | "brace" | "angle" |
    /// "dquote" | "squote" | "backtick"}`). Every `editor::MoveX` motion also exists as
    /// `editor::SelectX`. Returns `None` for any other name and for a `SelectBetween` with
    /// an unknown kind. Unknown or malformed arguments fall back to their defaults.
    ///
    /// A few commands need something a keymap cannot know, which the app fills in:
    ///
    /// - `editor::MovePageUp`/`MovePageDown`/`SelectPageUp`/`SelectPageDown` get `rows: 0`
    ///   unless a `"rows"` argument is given: use [`Command::with_page_rows`] or keep
    ///   [`crate::EditSettings::page_rows`] up to date. `center_cursor` is ignored.
    /// - `editor::ToggleComments` gets an empty prefix unless a `"prefix"` argument is
    ///   given: use [`Command::with_line_comment`].
    /// - `editor::ToggleBlockComments` gets empty delimiters unless `"start"` and `"end"`
    ///   are given: use [`Command::with_block_comment`].
    /// - `editor::Paste` gets an empty clipboard: build `Command::Paste` with the real one.
    pub fn from_action(name: &str, args: Option<&Value>) -> Option<Command> {
        let action = name.strip_prefix("editor::")?;
        if let Some(suffix) = action.strip_prefix("Move") {
            if let Some(motion) = motion_for(suffix, args, true) {
                return Some(Command::Move(motion));
            }
        }
        if let Some(suffix) = action.strip_prefix("Select") {
            if let Some(motion) = motion_for(suffix, args, false) {
                return Some(Command::Select(motion));
            }
        }
        let word_delete = || {
            (
                bool_arg(args, "ignore_newlines", false),
                bool_arg(args, "ignore_brackets", false),
            )
        };
        Some(match action {
            "GoToLine" => Command::Move(Motion::GoToLine {
                line: count_arg(args, "line"),
                column: count_arg(args, "column"),
            }),
            "SelectAll" => Command::SelectAll,
            "SelectLine" => Command::SelectLine,
            "SelectNext" => Command::SelectNext {
                replace_newest: bool_arg(args, "replace_newest", false),
            },
            "SelectPrevious" => Command::SelectPrevious {
                replace_newest: bool_arg(args, "replace_newest", false),
            },
            "SelectAllMatches" => Command::SelectAllMatches,
            "AddSelectionAbove" => Command::AddSelectionAbove {
                skip_soft_wrap: bool_arg(args, "skip_soft_wrap", true),
            },
            "AddSelectionBelow" => Command::AddSelectionBelow {
                skip_soft_wrap: bool_arg(args, "skip_soft_wrap", true),
            },
            "UndoSelection" => Command::UndoSelection,
            "RedoSelection" => Command::RedoSelection,
            "Cancel" => Command::Cancel,
            "SelectLargerSyntaxNode" => Command::SelectLargerSyntaxNode,
            "SelectSmallerSyntaxNode" => Command::SelectSmallerSyntaxNode,
            "SelectBetween" => Command::SelectBetween(PairKind::from_name(str_arg(args, "kind"))?),
            "Newline" => Command::Newline,
            "NewlineAbove" => Command::NewlineAbove,
            "NewlineBelow" => Command::NewlineBelow,
            "Backspace" => Command::Backspace,
            "Delete" => Command::Delete,
            "DeleteToPreviousWordStart" => {
                let (ignore_newlines, ignore_brackets) = word_delete();
                Command::DeleteToPreviousWordStart {
                    ignore_newlines,
                    ignore_brackets,
                }
            }
            "DeleteToNextWordEnd" => {
                let (ignore_newlines, ignore_brackets) = word_delete();
                Command::DeleteToNextWordEnd {
                    ignore_newlines,
                    ignore_brackets,
                }
            }
            "DeleteToPreviousSubwordStart" => {
                let (ignore_newlines, ignore_brackets) = word_delete();
                Command::DeleteToPreviousSubwordStart {
                    ignore_newlines,
                    ignore_brackets,
                }
            }
            "DeleteToNextSubwordEnd" => {
                let (ignore_newlines, ignore_brackets) = word_delete();
                Command::DeleteToNextSubwordEnd {
                    ignore_newlines,
                    ignore_brackets,
                }
            }
            "DeleteToBeginningOfLine" => Command::DeleteToBeginningOfLine {
                stop_at_indent: bool_arg(args, "stop_at_indent", false),
            },
            "DeleteToEndOfLine" => Command::DeleteToEndOfLine,
            "DeleteLine" => Command::DeleteLine,
            "Tab" => Command::Tab,
            "Backtab" => Command::Backtab,
            "Indent" => Command::Indent,
            "Outdent" => Command::Outdent,
            "MoveLineUp" => Command::MoveLineUp,
            "MoveLineDown" => Command::MoveLineDown,
            "DuplicateLineUp" => Command::DuplicateLineUp,
            "DuplicateLineDown" => Command::DuplicateLineDown,
            "JoinLines" => Command::JoinLines,
            "Transpose" => Command::Transpose,
            "ToggleComments" => Command::ToggleComments {
                prefix: match args {
                    Some(Value::Object(_)) => str_arg(args, "prefix").to_string(),
                    _ => String::new(),
                },
                comment_empty_lines: bool_arg(args, "comment_empty_lines", false),
                advance_downwards: bool_arg(args, "advance_downwards", false),
            },
            "ToggleBlockComments" => Command::ToggleBlockComments {
                start: match args {
                    Some(Value::Object(_)) => str_arg(args, "start").to_string(),
                    _ => String::new(),
                },
                end: match args {
                    Some(Value::Object(_)) => str_arg(args, "end").to_string(),
                    _ => String::new(),
                },
            },
            "Cut" => Command::Cut,
            "Copy" => Command::Copy,
            "Paste" => Command::Paste(Clipboard::default()),
            "KillRingCut" => Command::KillRingCut,
            "KillRingYank" => Command::KillRingYank,
            "Undo" => Command::Undo,
            "Redo" => Command::Redo,
            _ => return None,
        })
    }

    /// The same command made to extend the selection: a `Move` becomes a `Select` with the
    /// same motion. Every other command is returned unchanged. This is what a "selecting"
    /// mode applies to the movement keys.
    pub fn extending(self) -> Command {
        match self {
            Command::Move(motion) => Command::Select(motion),
            other => other,
        }
    }

    /// The same command with the page size filled in: `PageUp` and `PageDown` motions whose
    /// `rows` is 0 get `rows`. Every other command is returned unchanged.
    pub fn with_page_rows(self, rows: usize) -> Command {
        let fill = |motion: Motion| match motion {
            Motion::PageUp { rows: 0 } => Motion::PageUp { rows },
            Motion::PageDown { rows: 0 } => Motion::PageDown { rows },
            other => other,
        };
        match self {
            Command::Move(motion) => Command::Move(fill(motion)),
            Command::Select(motion) => Command::Select(fill(motion)),
            other => other,
        }
    }

    /// The same command with the language's line-comment prefix filled in: a
    /// `ToggleComments` whose prefix is empty gets `prefix`. Every other command is
    /// returned unchanged.
    pub fn with_line_comment(self, prefix: &str) -> Command {
        match self {
            Command::ToggleComments {
                prefix: old,
                comment_empty_lines,
                advance_downwards,
            } if old.is_empty() => Command::ToggleComments {
                prefix: prefix.to_string(),
                comment_empty_lines,
                advance_downwards,
            },
            other => other,
        }
    }

    /// The same command with the language's block-comment delimiters filled in: a
    /// `ToggleBlockComments` whose delimiters are both empty gets `start` and `end`. Every
    /// other command is returned unchanged.
    pub fn with_block_comment(self, start: &str, end: &str) -> Command {
        match self {
            Command::ToggleBlockComments {
                start: old_start,
                end: old_end,
            } if old_start.is_empty() && old_end.is_empty() => Command::ToggleBlockComments {
                start: start.to_string(),
                end: end.to_string(),
            },
            other => other,
        }
    }

    /// Whether the command can change the text. The app can use this to refuse edits to a
    /// read-only buffer before applying anything.
    pub fn modifies_text(&self) -> bool {
        !matches!(
            self,
            Command::Move(_)
                | Command::Select(_)
                | Command::SelectAll
                | Command::SelectLine
                | Command::SelectNext { .. }
                | Command::SelectPrevious { .. }
                | Command::SelectAllMatches
                | Command::AddSelectionAbove { .. }
                | Command::AddSelectionBelow { .. }
                | Command::UndoSelection
                | Command::RedoSelection
                | Command::Cancel
                | Command::SelectLargerSyntaxNode
                | Command::SelectSmallerSyntaxNode
                | Command::SelectBetween(_)
                | Command::Copy
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn action(name: &str) -> Option<Command> {
        Command::from_action(name, None)
    }

    fn action_with(name: &str, args: Value) -> Option<Command> {
        Command::from_action(name, Some(&args))
    }

    #[test]
    fn plain_movements() {
        assert_eq!(
            action("editor::MoveLeft"),
            Some(Command::Move(Motion::Left))
        );
        assert_eq!(
            action("editor::MoveRight"),
            Some(Command::Move(Motion::Right))
        );
        assert_eq!(action("editor::MoveUp"), Some(Command::Move(Motion::Up)));
        assert_eq!(
            action("editor::MoveDown"),
            Some(Command::Move(Motion::Down))
        );
        assert_eq!(
            action("editor::MoveToPreviousWordStart"),
            Some(Command::Move(Motion::PreviousWordStart))
        );
        assert_eq!(
            action("editor::MoveToNextWordEnd"),
            Some(Command::Move(Motion::NextWordEnd))
        );
        assert_eq!(
            action("editor::MoveToPreviousSubwordStart"),
            Some(Command::Move(Motion::PreviousSubwordStart))
        );
        assert_eq!(
            action("editor::MoveToNextSubwordEnd"),
            Some(Command::Move(Motion::NextSubwordEnd))
        );
        assert_eq!(
            action("editor::MoveToBeginning"),
            Some(Command::Move(Motion::Beginning))
        );
        assert_eq!(
            action("editor::MoveToEnd"),
            Some(Command::Move(Motion::End))
        );
        assert_eq!(
            action("editor::MoveToStartOfParagraph"),
            Some(Command::Move(Motion::StartOfParagraph))
        );
        assert_eq!(
            action("editor::MoveToEndOfParagraph"),
            Some(Command::Move(Motion::EndOfParagraph))
        );
        assert_eq!(
            action("editor::MoveToEnclosingBracket"),
            Some(Command::Move(Motion::EnclosingBracket))
        );
        assert_eq!(
            action("editor::MoveToNextWordStart"),
            Some(Command::Move(Motion::NextWordStart))
        );
        assert_eq!(
            action("editor::MoveToFirstNonBlank"),
            Some(Command::Move(Motion::FirstNonBlank))
        );
    }

    #[test]
    fn every_movement_has_a_selecting_form() {
        assert_eq!(
            action("editor::SelectLeft"),
            Some(Command::Select(Motion::Left))
        );
        assert_eq!(
            action("editor::SelectRight"),
            Some(Command::Select(Motion::Right))
        );
        assert_eq!(
            action("editor::SelectUp"),
            Some(Command::Select(Motion::Up))
        );
        assert_eq!(
            action("editor::SelectDown"),
            Some(Command::Select(Motion::Down))
        );
        assert_eq!(
            action("editor::SelectToPreviousWordStart"),
            Some(Command::Select(Motion::PreviousWordStart))
        );
        assert_eq!(
            action("editor::SelectToNextWordEnd"),
            Some(Command::Select(Motion::NextWordEnd))
        );
        assert_eq!(
            action("editor::SelectToPreviousSubwordStart"),
            Some(Command::Select(Motion::PreviousSubwordStart))
        );
        assert_eq!(
            action("editor::SelectToNextSubwordEnd"),
            Some(Command::Select(Motion::NextSubwordEnd))
        );
        assert_eq!(
            action("editor::SelectToBeginning"),
            Some(Command::Select(Motion::Beginning))
        );
        assert_eq!(
            action("editor::SelectToEnd"),
            Some(Command::Select(Motion::End))
        );
        assert_eq!(
            action("editor::SelectToStartOfParagraph"),
            Some(Command::Select(Motion::StartOfParagraph))
        );
        assert_eq!(
            action("editor::SelectToEndOfParagraph"),
            Some(Command::Select(Motion::EndOfParagraph))
        );
        assert_eq!(
            action("editor::SelectPageUp"),
            Some(Command::Select(Motion::PageUp { rows: 0 }))
        );
        assert_eq!(
            action("editor::SelectPageDown"),
            Some(Command::Select(Motion::PageDown { rows: 0 }))
        );
        assert_eq!(
            action_with("editor::SelectUpByLines", json!({"lines": 5})),
            Some(Command::Select(Motion::UpByLines { lines: 5 }))
        );
        assert_eq!(
            action_with("editor::SelectDownByLines", json!({"lines": 5})),
            Some(Command::Select(Motion::DownByLines { lines: 5 }))
        );
    }

    #[test]
    fn line_edge_arguments_and_defaults() {
        // Zed's defaults: moving stops at soft wraps, selecting does not.
        assert_eq!(
            action("editor::MoveToBeginningOfLine"),
            Some(Command::Move(Motion::BeginningOfLine {
                stop_at_soft_wraps: true,
                stop_at_indent: false
            }))
        );
        assert_eq!(
            action("editor::SelectToBeginningOfLine"),
            Some(Command::Select(Motion::BeginningOfLine {
                stop_at_soft_wraps: false,
                stop_at_indent: false
            }))
        );
        assert_eq!(
            action("editor::MoveToEndOfLine"),
            Some(Command::Move(Motion::EndOfLine {
                stop_at_soft_wraps: true
            }))
        );
        assert_eq!(
            action("editor::SelectToEndOfLine"),
            Some(Command::Select(Motion::EndOfLine {
                stop_at_soft_wraps: false
            }))
        );
        assert_eq!(
            action_with(
                "editor::MoveToBeginningOfLine",
                json!({"stop_at_soft_wraps": false, "stop_at_indent": true})
            ),
            Some(Command::Move(Motion::BeginningOfLine {
                stop_at_soft_wraps: false,
                stop_at_indent: true
            }))
        );
        assert_eq!(
            action_with(
                "editor::SelectToBeginningOfLine",
                json!({"stop_at_soft_wraps": true, "stop_at_indent": true})
            ),
            Some(Command::Select(Motion::BeginningOfLine {
                stop_at_soft_wraps: true,
                stop_at_indent: true
            }))
        );
        assert_eq!(
            action_with(
                "editor::SelectToEndOfLine",
                json!({"stop_at_soft_wraps": true})
            ),
            Some(Command::Select(Motion::EndOfLine {
                stop_at_soft_wraps: true
            }))
        );
    }

    #[test]
    fn counts_and_pages() {
        assert_eq!(
            action_with("editor::MoveUpByLines", json!({"lines": 5})),
            Some(Command::Move(Motion::UpByLines { lines: 5 }))
        );
        assert_eq!(
            action_with("editor::MoveDownByLines", json!({"lines": 12})),
            Some(Command::Move(Motion::DownByLines { lines: 12 }))
        );
        assert_eq!(
            action("editor::MoveUpByLines"),
            Some(Command::Move(Motion::UpByLines { lines: 0 }))
        );
        assert_eq!(
            action_with("editor::MoveUpByLines", json!({"lines": -3})),
            Some(Command::Move(Motion::UpByLines { lines: 0 }))
        );
        assert_eq!(
            action_with("editor::MovePageDown", json!({"center_cursor": true})),
            Some(Command::Move(Motion::PageDown { rows: 0 }))
        );
        assert_eq!(
            action_with("editor::MovePageUp", json!({"rows": 30})),
            Some(Command::Move(Motion::PageUp { rows: 30 }))
        );
        assert_eq!(
            action_with("editor::GoToLine", json!({"line": 12, "column": 4})),
            Some(Command::Move(Motion::GoToLine {
                line: 12,
                column: 4
            }))
        );
        assert_eq!(
            action("editor::GoToLine"),
            Some(Command::Move(Motion::GoToLine { line: 0, column: 0 }))
        );
    }

    #[test]
    fn selection_commands() {
        assert_eq!(action("editor::SelectAll"), Some(Command::SelectAll));
        assert_eq!(action("editor::SelectLine"), Some(Command::SelectLine));
        assert_eq!(
            action("editor::SelectNext"),
            Some(Command::SelectNext {
                replace_newest: false
            })
        );
        assert_eq!(
            action_with("editor::SelectNext", json!({"replace_newest": true})),
            Some(Command::SelectNext {
                replace_newest: true
            })
        );
        assert_eq!(
            action_with("editor::SelectPrevious", json!({"replace_newest": false})),
            Some(Command::SelectPrevious {
                replace_newest: false
            })
        );
        assert_eq!(
            action("editor::SelectAllMatches"),
            Some(Command::SelectAllMatches)
        );
        assert_eq!(
            action("editor::AddSelectionAbove"),
            Some(Command::AddSelectionAbove {
                skip_soft_wrap: true
            })
        );
        assert_eq!(
            action_with(
                "editor::AddSelectionBelow",
                json!({"skip_soft_wrap": false})
            ),
            Some(Command::AddSelectionBelow {
                skip_soft_wrap: false
            })
        );
        assert_eq!(
            action("editor::UndoSelection"),
            Some(Command::UndoSelection)
        );
        assert_eq!(
            action("editor::RedoSelection"),
            Some(Command::RedoSelection)
        );
        assert_eq!(action("editor::Cancel"), Some(Command::Cancel));
        assert_eq!(
            action("editor::SelectLargerSyntaxNode"),
            Some(Command::SelectLargerSyntaxNode)
        );
        assert_eq!(
            action("editor::SelectSmallerSyntaxNode"),
            Some(Command::SelectSmallerSyntaxNode)
        );
    }

    #[test]
    fn select_between_kinds() {
        for kind in PairKind::ALL {
            assert_eq!(
                action_with("editor::SelectBetween", json!({"kind": kind.name()})),
                Some(Command::SelectBetween(kind))
            );
        }
        assert_eq!(
            action_with("editor::SelectBetween", json!("brace")),
            Some(Command::SelectBetween(PairKind::Brace))
        );
        assert_eq!(
            action_with("editor::SelectBetween", json!({"kind": "nope"})),
            None
        );
        assert_eq!(action("editor::SelectBetween"), None);
    }

    #[test]
    fn editing_commands() {
        let simple = [
            ("Newline", Command::Newline),
            ("NewlineAbove", Command::NewlineAbove),
            ("NewlineBelow", Command::NewlineBelow),
            ("Backspace", Command::Backspace),
            ("Delete", Command::Delete),
            ("DeleteToEndOfLine", Command::DeleteToEndOfLine),
            ("DeleteLine", Command::DeleteLine),
            ("Tab", Command::Tab),
            ("Backtab", Command::Backtab),
            ("Indent", Command::Indent),
            ("Outdent", Command::Outdent),
            ("MoveLineUp", Command::MoveLineUp),
            ("MoveLineDown", Command::MoveLineDown),
            ("DuplicateLineUp", Command::DuplicateLineUp),
            ("DuplicateLineDown", Command::DuplicateLineDown),
            ("JoinLines", Command::JoinLines),
            ("Transpose", Command::Transpose),
            ("Cut", Command::Cut),
            ("Copy", Command::Copy),
            ("KillRingCut", Command::KillRingCut),
            ("KillRingYank", Command::KillRingYank),
            ("Undo", Command::Undo),
            ("Redo", Command::Redo),
        ];
        for (name, command) in simple {
            assert_eq!(action(&format!("editor::{name}")), Some(command), "{name}");
        }
        assert_eq!(
            action("editor::Paste"),
            Some(Command::Paste(Clipboard::default()))
        );
        assert_eq!(
            action("editor::DeleteToPreviousWordStart"),
            Some(Command::DeleteToPreviousWordStart {
                ignore_newlines: false,
                ignore_brackets: false
            })
        );
        assert_eq!(
            action_with(
                "editor::DeleteToNextWordEnd",
                json!({"ignore_newlines": true, "ignore_brackets": true})
            ),
            Some(Command::DeleteToNextWordEnd {
                ignore_newlines: true,
                ignore_brackets: true
            })
        );
        assert_eq!(
            action("editor::DeleteToPreviousSubwordStart"),
            Some(Command::DeleteToPreviousSubwordStart {
                ignore_newlines: false,
                ignore_brackets: false
            })
        );
        assert_eq!(
            action_with(
                "editor::DeleteToNextSubwordEnd",
                json!({"ignore_brackets": true})
            ),
            Some(Command::DeleteToNextSubwordEnd {
                ignore_newlines: false,
                ignore_brackets: true
            })
        );
        assert_eq!(
            action_with(
                "editor::DeleteToBeginningOfLine",
                json!({"stop_at_indent": true})
            ),
            Some(Command::DeleteToBeginningOfLine {
                stop_at_indent: true
            })
        );
    }

    #[test]
    fn comment_commands_take_their_markers_from_the_app() {
        let toggle = action_with(
            "editor::ToggleComments",
            json!({"advance_downwards": false, "comment_empty_lines": true}),
        )
        .unwrap();
        assert_eq!(
            toggle,
            Command::ToggleComments {
                prefix: String::new(),
                comment_empty_lines: true,
                advance_downwards: false
            }
        );
        assert_eq!(
            toggle.clone().with_line_comment("//"),
            Command::ToggleComments {
                prefix: "//".to_string(),
                comment_empty_lines: true,
                advance_downwards: false
            }
        );
        assert_eq!(
            action_with("editor::ToggleComments", json!({"prefix": "#"}))
                .unwrap()
                .with_line_comment("//"),
            Command::ToggleComments {
                prefix: "#".to_string(),
                comment_empty_lines: false,
                advance_downwards: false
            },
            "a prefix given in the keymap wins"
        );

        let block = action("editor::ToggleBlockComments").unwrap();
        assert_eq!(
            block,
            Command::ToggleBlockComments {
                start: String::new(),
                end: String::new()
            }
        );
        assert_eq!(
            block.with_block_comment("/*", "*/"),
            Command::ToggleBlockComments {
                start: "/*".to_string(),
                end: "*/".to_string()
            }
        );
        assert_eq!(
            action_with(
                "editor::ToggleBlockComments",
                json!({"start": "<!--", "end": "-->"})
            ),
            Some(Command::ToggleBlockComments {
                start: "<!--".to_string(),
                end: "-->".to_string()
            })
        );
        assert_eq!(Command::Copy.with_line_comment("//"), Command::Copy);
        assert_eq!(Command::Copy.with_block_comment("/*", "*/"), Command::Copy);
    }

    #[test]
    fn unknown_actions_are_none() {
        assert_eq!(action("editor::Fold"), None);
        assert_eq!(action("editor::MoveSideways"), None);
        assert_eq!(action("editor::SelectEnclosingSymbol"), None);
        assert_eq!(action("workspace::Save"), None);
        assert_eq!(action("MoveLeft"), None);
        assert_eq!(action(""), None);
        assert_eq!(action("editor::"), None);
    }

    #[test]
    fn malformed_arguments_fall_back_to_defaults() {
        assert_eq!(
            action_with("editor::MoveUpByLines", json!({"lines": "many"})),
            Some(Command::Move(Motion::UpByLines { lines: 0 }))
        );
        assert_eq!(
            action_with("editor::MoveUpByLines", json!(7)),
            Some(Command::Move(Motion::UpByLines { lines: 7 }))
        );
        assert_eq!(
            action_with("editor::SelectNext", json!({"replace_newest": "yes"})),
            Some(Command::SelectNext {
                replace_newest: false
            })
        );
        assert_eq!(
            action_with("editor::MoveLeft", json!([1, 2, 3])),
            Some(Command::Move(Motion::Left))
        );
        assert_eq!(
            action_with("editor::MoveToEndOfLine", json!(null)),
            Some(Command::Move(Motion::EndOfLine {
                stop_at_soft_wraps: true
            }))
        );
    }

    #[test]
    fn extending_turns_moves_into_selects() {
        assert_eq!(
            Command::Move(Motion::Left).extending(),
            Command::Select(Motion::Left)
        );
        assert_eq!(
            Command::Move(Motion::GoToLine { line: 3, column: 1 }).extending(),
            Command::Select(Motion::GoToLine { line: 3, column: 1 })
        );
        assert_eq!(
            Command::Select(Motion::Up).extending(),
            Command::Select(Motion::Up)
        );
        assert_eq!(Command::Copy.extending(), Command::Copy);
    }

    #[test]
    fn page_rows_are_filled_in_only_when_missing() {
        assert_eq!(
            Command::Move(Motion::PageUp { rows: 0 }).with_page_rows(40),
            Command::Move(Motion::PageUp { rows: 40 })
        );
        assert_eq!(
            Command::Select(Motion::PageDown { rows: 0 }).with_page_rows(40),
            Command::Select(Motion::PageDown { rows: 40 })
        );
        assert_eq!(
            Command::Move(Motion::PageDown { rows: 7 }).with_page_rows(40),
            Command::Move(Motion::PageDown { rows: 7 })
        );
        assert_eq!(
            Command::Move(Motion::Up).with_page_rows(40),
            Command::Move(Motion::Up)
        );
        assert_eq!(Command::Undo.with_page_rows(40), Command::Undo);
    }

    #[test]
    fn which_commands_modify_text() {
        assert!(!Command::Move(Motion::Left).modifies_text());
        assert!(!Command::Select(Motion::End).modifies_text());
        assert!(!Command::Copy.modifies_text());
        assert!(!Command::SelectBetween(PairKind::Paren).modifies_text());
        assert!(!Command::Cancel.modifies_text());
        assert!(Command::Insert("x".to_string()).modifies_text());
        assert!(Command::Cut.modifies_text());
        assert!(Command::Undo.modifies_text());
        assert!(Command::Paste(Clipboard::plain("x")).modifies_text());
        assert!(Command::KillRingCut.modifies_text());
    }

    #[test]
    fn clipboards() {
        let plain = Clipboard::plain("hello");
        assert_eq!(plain.text, "hello");
        assert!(!plain.linewise && plain.pieces.is_empty() && !plain.is_empty());
        assert!(Clipboard::default().is_empty());

        let piece = |text: &str, linewise: bool| ClipboardPiece {
            text: text.to_string(),
            linewise,
        };
        let mixed = Clipboard::from_pieces(vec![
            piece("one", false),
            piece("line\n", true),
            piece("two", false),
        ]);
        assert_eq!(mixed.text, "one\nline\ntwo");
        assert!(!mixed.linewise);
        let lines = Clipboard::from_pieces(vec![piece("a\n", true), piece("b\n", true)]);
        assert_eq!(lines.text, "a\nb\n");
        assert!(lines.linewise);
        assert!(!Clipboard::from_pieces(Vec::new()).linewise);
    }
}
