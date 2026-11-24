//! Reading the emulated screen: cells, the cursor, plain text, modes and links.
//!
//! Everything here is a function of alacritty's `Term`, with no pty involved, which is what
//! lets the unit tests below feed escape sequences straight into a terminal and look at the
//! result.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell as GridCell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape as AnsiCursorShape, NamedColor,
};
use bakedit_core::{Color, Rgb};

use crate::config::TermColors;
use crate::input::MouseModes;
use crate::keys::KeyModes;
use crate::links::{detect_links, hyperlink_target, LinkTarget};

/// How far past the edge of the screen a soft-wrapped line is followed when looking for
/// links. A link that starts further up than this is cut at the top of the screen.
const MAX_WRAP_ROWS: i32 = 16;

/// The kind of line drawn under a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Underline {
    /// No underline.
    #[default]
    None,
    /// A straight line (SGR 4).
    Single,
    /// Two lines (SGR 4:2).
    Double,
    /// A wavy line (SGR 4:3), the usual mark for a diagnostic.
    Curly,
    /// A dotted line (SGR 4:4).
    Dotted,
    /// A dashed line (SGR 4:5).
    Dashed,
}

/// One cell of the visible screen, as passed to the callback of
/// [`Terminal::render`](crate::Terminal::render).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell<'a> {
    /// The character. A blank for an empty cell and for the second half of a wide one.
    pub ch: char,
    /// Combining marks that go on top of `ch`, in the order they arrived.
    pub zerowidth: &'a [char],
    /// Text colour. [`Color::Default`] is the theme's terminal foreground.
    pub fg: Color,
    /// Background colour. [`Color::Default`] is the theme's terminal background.
    pub bg: Color,
    /// Colour of the underline, when it differs from the text's.
    pub underline_color: Option<Color>,
    /// Bold (SGR 1).
    pub bold: bool,
    /// Italic (SGR 3).
    pub italic: bool,
    /// Faint (SGR 2).
    pub dim: bool,
    /// The line under the cell, if any.
    pub underline: Underline,
    /// Struck through (SGR 9).
    pub strikeout: bool,
    /// Foreground and background swapped (SGR 7). The colours above are *not* swapped:
    /// the application swaps them when it draws.
    pub inverse: bool,
    /// Concealed text (SGR 8): draw the background only.
    pub hidden: bool,
    /// A double-width character. The cell to its right is its spacer.
    pub wide: bool,
    /// The second half of a wide character: nothing to draw here.
    pub wide_spacer: bool,
    /// Inside the selection made with
    /// [`Terminal::selection_start`](crate::Terminal::selection_start).
    pub selected: bool,
}

/// The shape of the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    /// A filled cell.
    Block,
    /// A line under the cell.
    Underline,
    /// A vertical bar at the cell's left edge.
    Bar,
    /// The outline of a cell: the terminal does not have the focus.
    Hollow,
}

/// Where the cursor is and what it looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Column, 0-based. The first half of a wide character when the cursor is on one.
    pub col: u16,
    /// Row of the visible screen, 0-based.
    pub row: u16,
    /// The shape the child asked for, or [`CursorShape::Hollow`] without the focus.
    pub shape: CursorShape,
    /// Whether the child asked for a blinking cursor.
    pub blinking: bool,
}

/// The modes the child has put the terminal in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TermModes {
    /// The child wants mouse events: any of modes 1000, 1002 and 1003.
    pub mouse_reporting: bool,
    /// Mode 1002: motion with a button held is reported.
    pub mouse_drag: bool,
    /// Mode 1003: all motion is reported.
    pub mouse_motion: bool,
    /// Mode 1006: mouse reports use the SGR encoding.
    pub sgr_mouse: bool,
    /// Mode 1005: legacy mouse reports with UTF-8 coordinates.
    pub utf8_mouse: bool,
    /// The alternate screen is showing (mode 1049): a full-screen program is running.
    pub alt_screen: bool,
    /// Mode 2004: pastes are bracketed.
    pub bracketed_paste: bool,
    /// Mode 1004: the child wants to know when the terminal gains and loses focus.
    pub focus_reporting: bool,
    /// Application cursor keys (DECCKM).
    pub app_cursor: bool,
    /// Application keypad (DECKPAM).
    pub app_keypad: bool,
    /// Mode 1007: the wheel sends arrow keys on the alternate screen.
    pub alternate_scroll: bool,
    /// The kitty keyboard protocol flags in effect (0 = legacy keys).
    pub kitty_flags: u8,
}

impl TermModes {
    pub(crate) fn from_mode(mode: TermMode) -> TermModes {
        let has = |flag: TermMode| mode.contains(flag);
        let mut kitty_flags = 0;
        for (flag, bit) in [
            (TermMode::DISAMBIGUATE_ESC_CODES, 1),
            (TermMode::REPORT_EVENT_TYPES, 2),
            (TermMode::REPORT_ALTERNATE_KEYS, 4),
            (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
            (TermMode::REPORT_ASSOCIATED_TEXT, 16),
        ] {
            if has(flag) {
                kitty_flags |= bit;
            }
        }
        TermModes {
            mouse_reporting: mode.intersects(TermMode::MOUSE_MODE),
            mouse_drag: has(TermMode::MOUSE_DRAG),
            mouse_motion: has(TermMode::MOUSE_MOTION),
            sgr_mouse: has(TermMode::SGR_MOUSE),
            utf8_mouse: has(TermMode::UTF8_MOUSE),
            alt_screen: has(TermMode::ALT_SCREEN),
            bracketed_paste: has(TermMode::BRACKETED_PASTE),
            focus_reporting: has(TermMode::FOCUS_IN_OUT),
            app_cursor: has(TermMode::APP_CURSOR),
            app_keypad: has(TermMode::APP_KEYPAD),
            alternate_scroll: has(TermMode::ALTERNATE_SCROLL),
            kitty_flags,
        }
    }

    /// The part of these modes that key encoding depends on.
    pub fn key_modes(&self) -> KeyModes {
        KeyModes {
            kitty_flags: self.kitty_flags,
            app_cursor: self.app_cursor,
            app_keypad: self.app_keypad,
        }
    }

    /// The part of these modes that mouse encoding depends on.
    pub fn mouse_modes(&self) -> MouseModes {
        MouseModes {
            click: self.mouse_reporting && !self.mouse_drag && !self.mouse_motion,
            drag: self.mouse_drag,
            motion: self.mouse_motion,
            sgr: self.sgr_mouse,
            utf8: self.utf8_mouse,
        }
    }
}

/// A link on the visible screen: one row's worth of it.
///
/// A link that wraps onto further rows comes as one `ScreenLink` per row, with the same
/// `target` and the same `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenLink {
    /// Row of the visible screen, 0-based.
    pub row: u16,
    /// First cell of the link on this row.
    pub col_start: u16,
    /// One past the last cell of the link on this row.
    pub col_end: u16,
    /// What the link points at.
    pub target: LinkTarget,
    /// The same for every segment of one link, different between links. Only meaningful
    /// within the result of a single call.
    pub id: u32,
}

impl ScreenLink {
    /// Whether the cell at `(col, row)` is part of this segment.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        self.row == row && self.col_start <= col && col < self.col_end
    }
}

fn rgb(color: alacritty_terminal::vte::ansi::Rgb) -> Rgb {
    Rgb::new(color.r, color.g, color.b)
}

/// Which default a cell colour falls back to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Foreground,
    Background,
}

/// Translate a grid colour for the application.
///
/// Palette entries and the defaults stay symbolic, so the application's theme decides what
/// they look like, unless the child redefined that very colour (OSC 4, 10, 11), in which
/// case the child's RGB value is passed on.
fn cell_color(color: AnsiColor, slot: Slot, overrides: &Colors, colors: &TermColors) -> Color {
    match color {
        AnsiColor::Spec(value) => Color::Rgb(rgb(value)),
        AnsiColor::Indexed(index) => match overrides[usize::from(index)] {
            Some(value) => Color::Rgb(rgb(value)),
            None => Color::Indexed(index),
        },
        AnsiColor::Named(name) => {
            if let Some(value) = overrides[name] {
                return Color::Rgb(rgb(value));
            }
            match name {
                NamedColor::Foreground
                | NamedColor::BrightForeground
                | NamedColor::DimForeground => match slot {
                    Slot::Foreground => Color::Default,
                    Slot::Background => Color::Rgb(colors.fg),
                },
                NamedColor::Background => match slot {
                    Slot::Background => Color::Default,
                    Slot::Foreground => Color::Rgb(colors.bg),
                },
                NamedColor::Cursor => Color::Rgb(colors.cursor),
                // The dim variants are the same eight colours; the cell's `dim` says the rest.
                dim if (dim as usize) >= NamedColor::DimBlack as usize => {
                    Color::Indexed((dim as usize - NamedColor::DimBlack as usize) as u8)
                }
                ansi => Color::Indexed(ansi as u8),
            }
        }
    }
}

fn underline_of(flags: Flags) -> Underline {
    if flags.contains(Flags::UNDERCURL) {
        Underline::Curly
    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
        Underline::Double
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        Underline::Dotted
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        Underline::Dashed
    } else if flags.contains(Flags::UNDERLINE) {
        Underline::Single
    } else {
        Underline::None
    }
}

/// The character to show for a cell. A tab is stored in the cell it started at; on screen
/// it is blank like the cells it skipped.
fn shown_char(cell: &GridCell) -> char {
    if cell.c == '\t' {
        ' '
    } else {
        cell.c
    }
}

/// How many lines the view is scrolled back.
///
/// alacritty_terminal 0.26 can report more than there is history: when a scroll region
/// that does not start at the top row scrolls while the view is scrolled back, the view is
/// moved though no line was added to the history. Indexing the grid with such an offset
/// reads outside of it, so everything here goes through this clamp instead.
pub(crate) fn view_offset<T>(term: &Term<T>) -> usize {
    term.grid().display_offset().min(term.history_size())
}

/// The grid line shown on `row` of the screen.
fn line_of_row<T>(term: &Term<T>, row: usize) -> Line {
    Line(row as i32 - view_offset(term) as i32)
}

/// Visit every cell of the visible screen, row by row.
pub(crate) fn render<T>(
    term: &Term<T>,
    colors: &TermColors,
    f: &mut dyn FnMut(u16, u16, &Cell<'_>),
) {
    let grid = term.grid();
    let overrides = term.colors();
    let selection = term
        .selection
        .as_ref()
        .and_then(|selection| selection.to_range(term));
    for row in 0..term.screen_lines() {
        let line = line_of_row(term, row);
        for (col, cell) in grid[line][..].iter().enumerate() {
            let flags = cell.flags;
            let wide = flags.contains(Flags::WIDE_CHAR);
            let wide_spacer = flags.contains(Flags::WIDE_CHAR_SPACER);
            let selected = selection.is_some_and(|range| {
                // Both halves of a wide character are selected when either is.
                let covers = |col: usize| range.contains(Point::new(line, Column(col)));
                covers(col)
                    || (wide && covers(col + 1))
                    || (wide_spacer && col > 0 && covers(col - 1))
            });
            let view = Cell {
                ch: shown_char(cell),
                zerowidth: cell.zerowidth().unwrap_or(&[]),
                fg: cell_color(cell.fg, Slot::Foreground, overrides, colors),
                bg: cell_color(cell.bg, Slot::Background, overrides, colors),
                underline_color: cell
                    .underline_color()
                    .map(|color| cell_color(color, Slot::Foreground, overrides, colors)),
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                dim: flags.contains(Flags::DIM),
                underline: underline_of(flags),
                strikeout: flags.contains(Flags::STRIKEOUT),
                inverse: flags.contains(Flags::INVERSE),
                hidden: flags.contains(Flags::HIDDEN),
                wide,
                wide_spacer,
                selected,
            };
            f(col as u16, row as u16, &view);
        }
    }
}

/// The cursor, unless the child hid it or it is scrolled off the screen.
pub(crate) fn cursor<T>(term: &Term<T>, focused: bool) -> Option<Cursor> {
    if !term.mode().contains(TermMode::SHOW_CURSOR) {
        return None;
    }
    let style = term.cursor_style();
    let shape = match style.shape {
        AnsiCursorShape::Hidden => return None,
        _ if !focused => CursorShape::Hollow,
        AnsiCursorShape::Block => CursorShape::Block,
        AnsiCursorShape::Underline => CursorShape::Underline,
        AnsiCursorShape::Beam => CursorShape::Bar,
        AnsiCursorShape::HollowBlock => CursorShape::Hollow,
    };
    let grid = term.grid();
    let mut point = grid.cursor.point;
    let row = point.line.0 + view_offset(term) as i32;
    if row < 0 || row >= term.screen_lines() as i32 {
        return None;
    }
    if point.column.0 > 0 && grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) {
        point.column.0 -= 1;
    }
    Some(Cursor {
        col: point.column.0 as u16,
        row: row as u16,
        shape,
        blinking: style.blinking,
    })
}

/// The text of one grid line, without the trailing blanks.
fn line_text<T>(term: &Term<T>, line: Line) -> String {
    let mut text = String::new();
    for cell in &term.grid()[line][..] {
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        text.push(shown_char(cell));
        text.extend(cell.zerowidth().unwrap_or(&[]));
    }
    text.truncate(text.trim_end_matches(' ').len());
    text
}

/// The text of one row of the visible screen. Empty for a row that is not on the screen.
pub(crate) fn row_text<T>(term: &Term<T>, row: u16) -> String {
    if usize::from(row) >= term.screen_lines() {
        return String::new();
    }
    line_text(term, line_of_row(term, usize::from(row)))
}

/// The text of the visible screen: its rows, joined by newlines.
pub(crate) fn screen_text<T>(term: &Term<T>) -> String {
    let rows: Vec<String> = (0..term.screen_lines())
        .map(|row| line_text(term, line_of_row(term, row)))
        .collect();
    rows.join("\n")
}

/// The grid point under the cell at `(col, row)` of the visible screen, clamped to it.
pub(crate) fn point_at<T>(term: &Term<T>, col: u16, row: u16) -> Point {
    let row = usize::from(row).min(term.screen_lines() - 1);
    let col = usize::from(col).min(term.columns() - 1);
    Point::new(line_of_row(term, row), Column(col))
}

/// How a selection grows from where it was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// Cell by cell, from the start cell to the cell under the pointer, both included.
    Simple,
    /// Whole words: the selection grows to the word boundaries on both sides, where a word
    /// ends at a blank, a quote, a bracket, a comma, a colon or a pane border.
    Word,
    /// Whole lines, following soft wraps.
    Line,
}

/// A selection the user is dragging out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Drag {
    kind: SelectionType,
    /// The cell the selection started at, as of the last update.
    origin: Point,
    /// Whether the pointer was before the origin at the last update.
    backward: bool,
}

/// Start a selection at the cell `(col, row)` of the visible screen, replacing any other.
/// A cell-by-cell selection is empty until it is updated; a word or line selection covers
/// the word or line at once.
pub(crate) fn selection_start<T>(
    term: &mut Term<T>,
    col: u16,
    row: u16,
    kind: SelectionKind,
) -> Drag {
    let origin = point_at(term, col, row);
    let kind = match kind {
        SelectionKind::Simple => SelectionType::Simple,
        SelectionKind::Word => SelectionType::Semantic,
        SelectionKind::Line => SelectionType::Lines,
    };
    term.selection = Some(Selection::new(kind, origin, Side::Left));
    Drag {
        kind,
        origin,
        backward: false,
    }
}

/// Move the end of the selection to the cell `(col, row)`. Returns `false`, changing
/// nothing, when the selection is gone: the terminal drops it when the text under it is
/// erased or scrolls away.
pub(crate) fn selection_update<T>(term: &mut Term<T>, drag: &mut Drag, col: u16, row: u16) -> bool {
    let point = point_at(term, col, row);
    if drag.kind != SelectionType::Simple {
        return match term.selection.as_mut() {
            Some(selection) => {
                selection.update(point, Side::Right);
                true
            }
            None => false,
        };
    }
    let Some(selection) = term.selection.as_ref() else {
        return false;
    };
    // alacritty selects between the *sides* of two cells, which a caller that only knows
    // cells cannot name. To have both end cells included whichever way the user drags, the
    // selection is rebuilt from its origin. The origin is read back from the terminal
    // because output scrolls a selection along with the text it covers.
    let origin = match selection.to_range(term) {
        Some(range) if drag.backward => range.end,
        Some(range) => range.start,
        None => drag.origin,
    };
    let backward = point < origin;
    let (origin_side, end_side) = if backward {
        (Side::Right, Side::Left)
    } else {
        (Side::Left, Side::Right)
    };
    let mut rebuilt = Selection::new(SelectionType::Simple, origin, origin_side);
    rebuilt.update(point, end_side);
    term.selection = Some(rebuilt);
    drag.origin = origin;
    drag.backward = backward;
    true
}

/// The selected text, or `None` when nothing is selected.
///
/// Rows are joined by newlines, except where a row is the soft-wrapped continuation of the
/// one before; blanks after the last character of a row are left out; a line selection
/// ends with a newline.
///
/// This does what alacritty's `Term::selection_to_string` does. That function is not used
/// because in 0.26 it reads the row *above* a row that ends in the placeholder for a wide
/// character that did not fit, which is the wrong row, and outside the grid when there is
/// no row above.
pub(crate) fn selection_text<T>(term: &Term<T>) -> Option<String> {
    let selection = term.selection.as_ref()?;
    let range = selection.to_range(term)?;
    let grid = term.grid();
    let last = term.last_column().0;
    let first_line = range.start.line.max(term.topmost_line());
    let last_line = range.end.line.min(term.bottommost_line());

    let mut text = String::new();
    let mut line = first_line;
    while line <= last_line {
        let row = &grid[line][..];
        let mut from = if line == range.start.line {
            range.start.column.0.min(last)
        } else {
            0
        };
        let to = if line == range.end.line {
            range.end.column.0.min(last)
        } else {
            last
        };
        // Starting on the second half of a wide character takes the character.
        if from > 0 && row[from].flags.contains(Flags::WIDE_CHAR_SPACER) {
            from -= 1;
        }
        // A soft-wrapped row is text to its edge; any other ends at its last character.
        let content = if row[last].flags.contains(Flags::WRAPLINE) {
            row.len()
        } else {
            row.iter()
                .rposition(|cell| !is_blank(cell))
                .map_or(0, |index| index + 1)
        };
        let length = content.min(to + 1);
        let mut skipping_tab = false;
        for (col, cell) in row.iter().enumerate().take(length).skip(from) {
            // A tab is one character in the cell it started at; the blanks up to the next
            // tab stop (every eighth column, unless the child moved them) belong to it.
            if skipping_tab && col % 8 != 0 && cell.c == ' ' {
                continue;
            }
            skipping_tab = cell.c == '\t';
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            text.push(cell.c);
            text.extend(cell.zerowidth().unwrap_or(&[]));
        }
        // The row ends here unless its last selected cell says the text wraps on.
        if to >= last && (length == 0 || !row[length - 1].flags.contains(Flags::WRAPLINE)) {
            text.push('\n');
        }
        // The selection ends on the placeholder for a wide character that moved to the
        // next row: the character is what was meant.
        let placeholder =
            length == row.len() && row[last].flags.contains(Flags::LEADING_WIDE_CHAR_SPACER);
        if placeholder && line == range.end.line && line < term.bottommost_line() {
            text.push(grid[line + 1i32][Column(0)].c);
        }
        line += 1i32;
    }
    if text.ends_with('\n') {
        text.pop();
    }
    if selection.ty == SelectionType::Lines {
        text.push('\n');
    }
    (!text.is_empty()).then_some(text)
}

/// Whether a cell holds nothing: no character and no combining mark.
fn is_blank(cell: &GridCell) -> bool {
    cell.c == ' ' && cell.zerowidth().is_none_or(|marks| marks.is_empty())
}

/// The cell a character of a logical line sits in.
#[derive(Clone, Copy)]
struct CellRef {
    line: Line,
    col: usize,
    width: usize,
}

/// Every link on the visible screen.
///
/// Rows that are soft-wrapped continuations of each other are joined before detection, so
/// a path broken by the right edge is still one link. Cells written with an OSC 8
/// hyperlink are links to exactly what the program said, and take precedence over anything
/// detected in their text.
pub(crate) fn links<T>(term: &Term<T>) -> Vec<ScreenLink> {
    let grid = term.grid();
    let offset = view_offset(term) as i32;
    let top = Line(-offset);
    let bottom = Line(term.screen_lines() as i32 - 1 - offset);
    let last_column = term.last_column();
    let wraps = |line: Line| grid[line][last_column].flags.contains(Flags::WRAPLINE);

    let mut found = Vec::new();
    let mut next_id = 0u32;
    let mut explicit_ids: Vec<(String, u32)> = Vec::new();

    let mut line = top;
    while line <= bottom {
        // The logical line this row belongs to, following the wraps a little way off the
        // screen. Only the top row can have a beginning above the screen: every later
        // one starts where the previous logical line ended.
        let mut first = line;
        while line == top
            && first > term.topmost_line()
            && top.0 - first.0 < MAX_WRAP_ROWS
            && wraps(first - 1i32)
        {
            first -= 1i32;
        }
        let mut last = line;
        while last < term.bottommost_line() && last.0 - bottom.0 < MAX_WRAP_ROWS && wraps(last) {
            last += 1i32;
        }

        let mut text = String::new();
        let mut cells: Vec<CellRef> = Vec::new();
        let mut explicit: Vec<(CellRef, alacritty_terminal::term::cell::Hyperlink)> = Vec::new();
        let mut current = first;
        while current <= last {
            for (col, cell) in grid[current][..].iter().enumerate() {
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                // A resize can leave half a wide character in the last column.
                let wide = cell.flags.contains(Flags::WIDE_CHAR) && col < last_column.0;
                let place = CellRef {
                    line: current,
                    col,
                    width: if wide { 2 } else { 1 },
                };
                text.push(shown_char(cell));
                cells.push(place);
                for mark in cell.zerowidth().unwrap_or(&[]) {
                    text.push(*mark);
                    cells.push(place);
                }
                if let Some(hyperlink) = cell.hyperlink() {
                    explicit.push((place, hyperlink));
                }
            }
            current += 1i32;
        }

        let on_screen = |place: &CellRef| place.line >= top && place.line <= bottom;
        let is_explicit = |place: &CellRef| {
            explicit
                .iter()
                .any(|(other, _)| other.line == place.line && other.col == place.col)
        };

        for span in detect_links(&text) {
            let places = &cells[span.start.min(cells.len())..span.end.min(cells.len())];
            if places.iter().any(is_explicit) {
                continue;
            }
            let before = found.len();
            push_segments(
                &mut found,
                places.iter().filter(|place| on_screen(place)),
                offset,
                &span.target,
                next_id,
            );
            if found.len() > before {
                next_id += 1;
            }
        }

        // Runs of cells that share one OSC 8 hyperlink.
        let mut start = 0;
        while start < explicit.len() {
            let hyperlink = &explicit[start].1;
            let mut end = start + 1;
            while end < explicit.len() && explicit[end].1 == *hyperlink {
                end += 1;
            }
            if let Some(target) = hyperlink_target(hyperlink.uri()) {
                // Cells of one hyperlink share its id wherever they are on the screen.
                let key = format!("{}\n{}", hyperlink.id(), hyperlink.uri());
                let id = match explicit_ids.iter().find(|(known, _)| *known == key) {
                    Some((_, id)) => *id,
                    None => {
                        explicit_ids.push((key, next_id));
                        next_id += 1;
                        next_id - 1
                    }
                };
                push_segments(
                    &mut found,
                    explicit[start..end]
                        .iter()
                        .map(|(place, _)| place)
                        .filter(|place| on_screen(place)),
                    offset,
                    &target,
                    id,
                );
            }
            start = end;
        }

        line = last + 1i32;
    }
    collect_wrapped_underlined_links(term, top, bottom, offset, &mut found, &mut next_id);
    found.sort_by_key(|link| (link.row, link.col_start));
    found
}

/// Reconstruct underlined links that were hard-wrapped across rows inside a split pane
/// (for example, `agy` or `claude` running inside `herdr`/`tmux`, where box-drawing pane
/// borders break `Flags::WRAPLINE` and OSC-8 hyperlinks may not be forwarded).
fn collect_wrapped_underlined_links<T>(
    term: &Term<T>,
    top: Line,
    bottom: Line,
    offset: i32,
    found: &mut Vec<ScreenLink>,
    next_id: &mut u32,
) {
    let grid = term.grid();
    let cols = grid.columns();
    if cols == 0 {
        return;
    }
    let underline_flags = Flags::UNDERLINE
        | Flags::DOUBLE_UNDERLINE
        | Flags::UNDERCURL
        | Flags::DOTTED_UNDERLINE
        | Flags::DASHED_UNDERLINE;
    let is_border = |c: char| matches!(c, '│' | '┃' | '║' | '┆' | '┇' | '┊' | '┋' | '▏' | '▕');

    #[derive(Clone)]
    struct Run {
        line: Line,
        /// Left and right pane boundaries (exclusive of border chars) enclosing this run.
        pane_left: usize,
        pane_right: usize,
        /// True when nothing non-whitespace follows this run before `pane_right`.
        reaches_right: bool,
        /// True when nothing non-whitespace precedes this run after `pane_left`.
        starts_at_left: bool,
        text: String,
        cells: Vec<CellRef>,
    }

    let mut runs: Vec<Run> = Vec::new();
    let mut cur = top;
    while cur <= bottom {
        let row = &grid[cur];
        let mut col = 0;
        while col < cols {
            let cell = &row[alacritty_terminal::index::Column(col)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                || !cell.flags.intersects(underline_flags)
                || cell.hyperlink().is_some()
                || shown_char(cell).is_whitespace()
                || is_border(shown_char(cell))
            {
                col += 1;
                continue;
            }
            let start_col = col;
            let mut text = String::new();
            let mut cells = Vec::new();
            while col < cols {
                let c = &row[alacritty_terminal::index::Column(col)];
                if c.flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    col += 1;
                    continue;
                }
                let ch = shown_char(c);
                if !c.flags.intersects(underline_flags)
                    || c.hyperlink().is_some()
                    || ch.is_whitespace()
                    || is_border(ch)
                {
                    break;
                }
                let wide = c.flags.contains(Flags::WIDE_CHAR) && col + 1 < cols;
                let place = CellRef {
                    line: cur,
                    col,
                    width: if wide { 2 } else { 1 },
                };
                text.push(ch);
                cells.push(place);
                for mark in c.zerowidth().unwrap_or(&[]) {
                    text.push(*mark);
                    cells.push(place);
                }
                col += place.width;
            }
            let end_col = col;
            if text.is_empty() {
                continue;
            }
            // Find enclosing vertical pane borders on this row.
            let mut pane_left = 0;
            for c_idx in (0..start_col).rev() {
                if is_border(shown_char(&row[alacritty_terminal::index::Column(c_idx)])) {
                    pane_left = c_idx + 1;
                    break;
                }
            }
            let mut pane_right = cols;
            for c_idx in end_col..cols {
                if is_border(shown_char(&row[alacritty_terminal::index::Column(c_idx)])) {
                    pane_right = c_idx;
                    break;
                }
            }
            let starts_at_left = (pane_left..start_col)
                .all(|c_idx| shown_char(&row[alacritty_terminal::index::Column(c_idx)]).is_whitespace());
            let reaches_right = (end_col..pane_right)
                .all(|c_idx| shown_char(&row[alacritty_terminal::index::Column(c_idx)]).is_whitespace());

            runs.push(Run {
                line: cur,
                pane_left,
                pane_right,
                reaches_right,
                starts_at_left,
                text,
                cells,
            });
        }
        cur += 1i32;
    }

    let mut used = vec![false; runs.len()];
    for i in 0..runs.len() {
        if used[i] || !runs[i].reaches_right {
            continue;
        }
        let mut chain = vec![i];
        used[i] = true;
        let mut last_idx = i;
        while runs[last_idx].reaches_right {
            let next_line = runs[last_idx].line + 1i32;
            let p_left = runs[last_idx].pane_left;
            let p_right = runs[last_idx].pane_right;
            let Some(next_idx) = runs.iter().enumerate().position(|(j, r)| {
                !used[j]
                    && r.line == next_line
                    && r.starts_at_left
                    && r.pane_left == p_left
                    && r.pane_right == p_right
            }) else {
                break;
            };
            used[next_idx] = true;
            chain.push(next_idx);
            last_idx = next_idx;
        }
        if chain.len() < 2 {
            continue;
        }
        let mut joined = String::new();
        let mut places = Vec::new();
        for &idx in &chain {
            joined.push_str(&runs[idx].text);
            places.extend_from_slice(&runs[idx].cells);
        }
        for span in detect_links(&joined) {
            let slice = &places[span.start.min(places.len())..span.end.min(places.len())];
            let first_line = slice.first().map(|p| p.line);
            let last_line = slice.last().map(|p| p.line);
            if first_line.is_none() || first_line == last_line {
                continue;
            }
            // Remove single-row partial links that overlap any cell of this reconstructed link.
            found.retain(|existing| {
                !slice.iter().any(|p| {
                    let row = (p.line.0 + offset) as u16;
                    existing.row == row
                        && (p.col as u16) < existing.col_end
                        && ((p.col + p.width) as u16) > existing.col_start
                })
            });
            let before = found.len();
            push_segments(found, slice.iter(), offset, &span.target, *next_id);
            if found.len() > before {
                *next_id += 1;
            }
        }
    }
}

/// Turn the cells of one link into one [`ScreenLink`] per row and stretch of adjacent cells.
fn push_segments<'a>(
    found: &mut Vec<ScreenLink>,
    places: impl Iterator<Item = &'a CellRef>,
    offset: i32,
    target: &LinkTarget,
    id: u32,
) {
    let mut open: Option<(Line, usize, usize)> = None;
    let close = |segment: Option<(Line, usize, usize)>, found: &mut Vec<ScreenLink>| {
        if let Some((line, start, end)) = segment {
            found.push(ScreenLink {
                row: (line.0 + offset) as u16,
                col_start: start as u16,
                col_end: end as u16,
                target: target.clone(),
                id,
            });
        }
    };
    for place in places {
        match open {
            Some((line, start, end)) if line == place.line && place.col <= end => {
                open = Some((line, start, end.max(place.col + place.width)));
            }
            _ => {
                close(open.take(), found);
                open = Some((place.line, place.col, place.col + place.width));
            }
        }
    }
    close(open, found);
}

#[cfg(test)]
pub(crate) mod testing {
    //! A terminal with no process behind it, fed by hand.

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::Processor;

    pub(crate) struct Size(pub usize, pub usize);

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }

        fn screen_lines(&self) -> usize {
            self.1
        }

        fn columns(&self) -> usize {
            self.0
        }
    }

    /// A `cols` x `rows` terminal that has received `output`.
    pub(crate) fn term_with(cols: usize, rows: usize, output: &str) -> Term<VoidListener> {
        let config = Config {
            kitty_keyboard: true,
            ..Config::default()
        };
        let mut term = Term::new(config, &Size(cols, rows), VoidListener);
        feed(&mut term, output);
        term
    }

    pub(crate) fn feed(term: &mut Term<VoidListener>, output: &str) {
        let mut parser: Processor = Processor::new();
        parser.advance(term, output.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{feed, term_with};
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Scroll;

    /// The cells of one row, as owned copies of what `render` hands out.
    #[derive(Debug, Clone, PartialEq)]
    struct Seen {
        ch: char,
        marks: Vec<char>,
        fg: Color,
        bg: Color,
        underline_color: Option<Color>,
        bold: bool,
        italic: bool,
        dim: bool,
        underline: Underline,
        strikeout: bool,
        inverse: bool,
        hidden: bool,
        wide: bool,
        wide_spacer: bool,
        selected: bool,
    }

    fn cells(term: &Term<VoidListener>) -> Vec<Vec<Seen>> {
        let mut rows: Vec<Vec<Seen>> = Vec::new();
        let mut order = Vec::new();
        render(term, &TermColors::default(), &mut |col, row, cell| {
            order.push((row, col));
            if usize::from(row) == rows.len() {
                rows.push(Vec::new());
            }
            rows[usize::from(row)].push(Seen {
                ch: cell.ch,
                marks: cell.zerowidth.to_vec(),
                fg: cell.fg,
                bg: cell.bg,
                underline_color: cell.underline_color,
                bold: cell.bold,
                italic: cell.italic,
                dim: cell.dim,
                underline: cell.underline,
                strikeout: cell.strikeout,
                inverse: cell.inverse,
                hidden: cell.hidden,
                wide: cell.wide,
                wide_spacer: cell.wide_spacer,
                selected: cell.selected,
            });
        });
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted, "cells are visited in row order");
        rows
    }

    fn link(
        row: u16,
        col_start: u16,
        col_end: u16,
        path: &str,
        line: Option<u32>,
        id: u32,
    ) -> ScreenLink {
        ScreenLink {
            row,
            col_start,
            col_end,
            target: LinkTarget::Path {
                path: path.to_string(),
                line,
                col: None,
            },
            id,
        }
    }

    #[test]
    fn every_cell_is_visited_once() {
        let term = term_with(7, 3, "ab");
        let rows = cells(&term);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.len() == 7));
        assert_eq!(rows[0][0].ch, 'a');
        assert_eq!(rows[0][1].ch, 'b');
        assert_eq!(rows[0][2].ch, ' ');
        assert_eq!(rows[2][6].ch, ' ');
        let blank = &rows[1][3];
        assert_eq!((blank.fg, blank.bg), (Color::Default, Color::Default));
        assert!(!blank.bold && !blank.wide && !blank.selected);
    }

    #[test]
    fn colours_stay_symbolic_until_they_are_rgb() {
        let term = term_with(20, 2, "\x1b[31ma\x1b[92mb\x1b[38;5;208mc\x1b[38;2;1;2;3md\x1b[39;44me\x1b[48;5;17mf\x1b[48;2;9;8;7mg\x1b[0mh");
        let row = &cells(&term)[0];
        assert_eq!(row[0].fg, Color::Indexed(1));
        assert_eq!(row[1].fg, Color::Indexed(10));
        assert_eq!(row[2].fg, Color::Indexed(208));
        assert_eq!(row[3].fg, Color::Rgb(Rgb::new(1, 2, 3)));
        assert_eq!((row[4].fg, row[4].bg), (Color::Default, Color::Indexed(4)));
        assert_eq!(row[5].bg, Color::Indexed(17));
        assert_eq!(row[6].bg, Color::Rgb(Rgb::new(9, 8, 7)));
        assert_eq!((row[7].fg, row[7].bg), (Color::Default, Color::Default));
    }

    #[test]
    fn colours_the_child_redefined_come_through_as_rgb() {
        // OSC 4 for a palette entry, OSC 10 and 11 for the defaults.
        let mut term = term_with(
            10,
            1,
            "\x1b]4;1;rgb:10/20/30\x07\x1b[31ma\x1b[38;5;1mb\x1b[32mc\x1b[0md",
        );
        let row = &cells(&term)[0];
        assert_eq!(row[0].fg, Color::Rgb(Rgb::new(0x10, 0x20, 0x30)));
        assert_eq!(row[1].fg, Color::Rgb(Rgb::new(0x10, 0x20, 0x30)));
        assert_eq!(row[2].fg, Color::Indexed(2));
        assert_eq!((row[3].fg, row[3].bg), (Color::Default, Color::Default));

        feed(&mut term, "\x1b]10;#aabbcc\x07\x1b]11;#010203\x07");
        let row = &cells(&term)[0];
        assert_eq!(
            (row[3].fg, row[3].bg),
            (
                Color::Rgb(Rgb::new(0xaa, 0xbb, 0xcc)),
                Color::Rgb(Rgb::new(1, 2, 3))
            )
        );
        // Resetting hands the colour back to the theme.
        feed(&mut term, "\x1b]104;1\x07\x1b]110\x07\x1b]111\x07");
        let row = &cells(&term)[0];
        assert_eq!(row[0].fg, Color::Indexed(1));
        assert_eq!((row[3].fg, row[3].bg), (Color::Default, Color::Default));
    }

    #[test]
    fn attributes() {
        let term = term_with(
            20,
            1,
            "\x1b[1ma\x1b[0;3mb\x1b[0;2mc\x1b[0;9md\x1b[0;7me\x1b[0;8mf\x1b[0;1;3;7mg\x1b[0mh",
        );
        let row = &cells(&term)[0];
        assert!(row[0].bold && !row[0].italic);
        assert!(row[1].italic && !row[1].bold);
        assert!(row[2].dim);
        assert!(row[3].strikeout);
        assert!(row[4].inverse);
        // Inverse is a flag: the colours are not swapped here.
        assert_eq!((row[4].fg, row[4].bg), (Color::Default, Color::Default));
        assert!(row[5].hidden);
        assert_eq!(row[5].ch, 'f');
        assert!(row[6].bold && row[6].italic && row[6].inverse);
        assert!(
            !row[7].bold
                && !row[7].italic
                && !row[7].inverse
                && !row[7].dim
                && !row[7].hidden
                && !row[7].strikeout
        );
    }

    #[test]
    fn underline_styles_and_colours() {
        let term = term_with(20, 1, "\x1b[4ma\x1b[4:2mb\x1b[4:3mc\x1b[4:4md\x1b[4:5me\x1b[4:1mf\x1b[24mg\x1b[4;58;5;196mh\x1b[58;2;1;2;3mi\x1b[59mj");
        let row = &cells(&term)[0];
        let styles: Vec<Underline> = row[..7].iter().map(|cell| cell.underline).collect();
        assert_eq!(
            styles,
            [
                Underline::Single,
                Underline::Double,
                Underline::Curly,
                Underline::Dotted,
                Underline::Dashed,
                Underline::Single,
                Underline::None
            ]
        );
        assert_eq!(row[0].underline_color, None);
        assert_eq!(row[7].underline_color, Some(Color::Indexed(196)));
        assert_eq!(row[8].underline_color, Some(Color::Rgb(Rgb::new(1, 2, 3))));
        assert_eq!(
            (row[9].underline, row[9].underline_color),
            (Underline::Single, None)
        );
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let term = term_with(10, 2, "a日本b");
        let row = &cells(&term)[0];
        assert_eq!(row[0].ch, 'a');
        assert_eq!(
            (row[1].ch, row[1].wide, row[1].wide_spacer),
            ('日', true, false)
        );
        assert_eq!(
            (row[2].ch, row[2].wide, row[2].wide_spacer),
            (' ', false, true)
        );
        assert_eq!((row[3].ch, row[3].wide), ('本', true));
        assert!(row[4].wide_spacer);
        assert_eq!(
            (row[5].ch, row[5].wide, row[5].wide_spacer),
            ('b', false, false)
        );
        assert_eq!(row_text(&term, 0), "a日本b");
    }

    #[test]
    fn a_wide_character_that_does_not_fit_moves_to_the_next_row() {
        let term = term_with(4, 2, "abc日");
        let rows = cells(&term);
        // The last cell of the first row is a placeholder, not half a character.
        assert_eq!(
            (rows[0][3].ch, rows[0][3].wide, rows[0][3].wide_spacer),
            (' ', false, false)
        );
        assert_eq!((rows[1][0].ch, rows[1][0].wide), ('日', true));
        assert!(rows[1][1].wide_spacer);
        assert_eq!(row_text(&term, 0), "abc");
        assert_eq!(row_text(&term, 1), "日");
    }

    #[test]
    fn combining_marks_ride_on_their_base_character() {
        let term = term_with(10, 1, "e\u{301}x日\u{309a}");
        let row = &cells(&term)[0];
        assert_eq!(
            (row[0].ch, row[0].marks.as_slice()),
            ('e', &['\u{301}'][..])
        );
        assert_eq!((row[1].ch, row[1].marks.is_empty()), ('x', true));
        assert_eq!(
            (row[2].ch, row[2].marks.as_slice()),
            ('日', &['\u{309a}'][..])
        );
        assert_eq!(row_text(&term, 0), "e\u{301}x日\u{309a}");
    }

    #[test]
    fn tabs_are_blank_on_screen() {
        let term = term_with(20, 1, "a\tb");
        let row = &cells(&term)[0];
        assert!(row[1..8].iter().all(|cell| cell.ch == ' '));
        assert_eq!(row[8].ch, 'b');
        assert_eq!(row_text(&term, 0), "a       b");
    }

    #[test]
    fn text_is_trimmed_per_row() {
        let term = term_with(10, 4, "one  \r\n\r\n  three\r\nlast");
        assert_eq!(row_text(&term, 0), "one");
        assert_eq!(row_text(&term, 1), "");
        assert_eq!(row_text(&term, 2), "  three");
        assert_eq!(row_text(&term, 3), "last");
        assert_eq!(row_text(&term, 4), "");
        assert_eq!(row_text(&term, u16::MAX), "");
        assert_eq!(screen_text(&term), "one\n\n  three\nlast");
    }

    #[test]
    fn the_cursor_follows_the_output() {
        let mut term = term_with(10, 3, "ab");
        assert_eq!(
            cursor(&term, true),
            Some(Cursor {
                col: 2,
                row: 0,
                shape: CursorShape::Block,
                blinking: false
            })
        );
        feed(&mut term, "\r\nxyz");
        assert_eq!(cursor(&term, true).map(|c| (c.col, c.row)), Some((3, 1)));
        // DECSCUSR: 5 is a blinking bar, 4 a steady underline, 2 a steady block.
        feed(&mut term, "\x1b[5 q");
        assert_eq!(
            cursor(&term, true).map(|c| (c.shape, c.blinking)),
            Some((CursorShape::Bar, true))
        );
        feed(&mut term, "\x1b[4 q");
        assert_eq!(
            cursor(&term, true).map(|c| (c.shape, c.blinking)),
            Some((CursorShape::Underline, false))
        );
        feed(&mut term, "\x1b[2 q");
        assert_eq!(
            cursor(&term, true).map(|c| (c.shape, c.blinking)),
            Some((CursorShape::Block, false))
        );
        // Without the focus it is hollow, whatever the child asked for.
        assert_eq!(
            cursor(&term, false).map(|c| c.shape),
            Some(CursorShape::Hollow)
        );
        feed(&mut term, "\x1b[?25l");
        assert_eq!(cursor(&term, true), None);
        assert_eq!(cursor(&term, false), None);
        feed(&mut term, "\x1b[?25h");
        assert!(cursor(&term, true).is_some());
    }

    #[test]
    fn the_cursor_sits_on_the_first_half_of_a_wide_character() {
        let mut term = term_with(10, 1, "日");
        feed(&mut term, "\x1b[1;2H");
        assert_eq!(cursor(&term, true).map(|c| (c.col, c.row)), Some((0, 0)));
    }

    #[test]
    fn scrolling_back_moves_the_view_and_hides_the_cursor() {
        let output: String = (1..=10).map(|n| format!("line {n}\r\n")).collect();
        let mut term = term_with(10, 4, &output);
        assert_eq!(screen_text(&term), "line 8\nline 9\nline 10\n");
        assert_eq!(cursor(&term, true).map(|c| c.row), Some(3));
        term.scroll_display(Scroll::Delta(2));
        assert_eq!(screen_text(&term), "line 6\nline 7\nline 8\nline 9");
        assert_eq!(cursor(&term, true), None);
        term.scroll_display(Scroll::Delta(1));
        // One row of the live screen is still visible, but not the cursor's.
        assert_eq!(row_text(&term, 3), "line 8");
        assert_eq!(cursor(&term, true), None);
        term.scroll_display(Scroll::Top);
        assert_eq!(row_text(&term, 0), "line 1");
        term.scroll_display(Scroll::Bottom);
        assert_eq!(cursor(&term, true).map(|c| c.row), Some(3));
        let first = &cells(&term)[0];
        assert_eq!(first[5].ch, '8');
    }

    #[test]
    fn selection_marks_cells() {
        let mut term = term_with(10, 2, "hello 日本\r\nworld");
        let mut drag = selection_start(&mut term, 4, 0, SelectionKind::Simple);
        assert!(selection_update(&mut term, &mut drag, 1, 1));
        let rows = cells(&term);
        let marked: Vec<String> = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| if cell.selected { '#' } else { '.' })
                    .collect()
            })
            .collect();
        assert_eq!(marked, ["....######", "##........"]);
        assert_eq!(selection_text(&term).as_deref(), Some("o 日本\nwo"));

        // Ending on the first half of a wide character selects both halves.
        let mut drag = selection_start(&mut term, 5, 0, SelectionKind::Simple);
        assert!(selection_update(&mut term, &mut drag, 6, 0));
        let row = &cells(&term)[0];
        assert!(row[5].selected && row[6].selected && row[7].selected);
        assert!(!row[8].selected);
        assert_eq!(selection_text(&term).as_deref(), Some(" 日"));
        // So does ending on its second half.
        let mut drag = selection_start(&mut term, 5, 0, SelectionKind::Simple);
        assert!(selection_update(&mut term, &mut drag, 7, 0));
        let row = &cells(&term)[0];
        assert!(row[6].selected && row[7].selected && !row[8].selected);
        assert_eq!(selection_text(&term).as_deref(), Some(" 日"));
        term.selection = None;
        assert!(cells(&term).iter().flatten().all(|cell| !cell.selected));
    }

    fn selected(term: &Term<VoidListener>) -> Vec<String> {
        cells(term)
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| if cell.selected { '#' } else { '.' })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_drag_includes_both_end_cells_in_either_direction() {
        let mut term = term_with(10, 2, "abcdefghij\r\nklmnopqrst");
        let mut drag = selection_start(&mut term, 3, 0, SelectionKind::Simple);
        // A press without a drag selects nothing.
        assert_eq!(selection_text(&term), None);
        assert_eq!(selected(&term), ["..........", ".........."]);

        assert!(selection_update(&mut term, &mut drag, 5, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("def"));
        assert_eq!(selected(&term), ["...###....", ".........."]);
        // Back onto the start cell: just that cell.
        assert!(selection_update(&mut term, &mut drag, 3, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("d"));
        // Before the start: the start cell stays in.
        assert!(selection_update(&mut term, &mut drag, 1, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("bcd"));
        assert_eq!(selected(&term), [".###......", ".........."]);
        // And forward again, onto the next row.
        assert!(selection_update(&mut term, &mut drag, 2, 1));
        assert_eq!(selection_text(&term).as_deref(), Some("defghij\nklm"));
        assert_eq!(selected(&term), ["...#######", "###......."]);
        // Positions off the screen are clamped to it.
        assert!(selection_update(&mut term, &mut drag, 500, 500));
        assert_eq!(
            selection_text(&term).as_deref(),
            Some("defghij\nklmnopqrst")
        );
    }

    #[test]
    fn a_backward_drag_from_a_later_row() {
        let mut term = term_with(10, 2, "abcdefghij\r\nklmnopqrst");
        let mut drag = selection_start(&mut term, 1, 1, SelectionKind::Simple);
        assert!(selection_update(&mut term, &mut drag, 8, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("ij\nkl"));
        assert_eq!(selected(&term), ["........##", "##........"]);
    }

    #[test]
    fn word_and_line_selections_cover_their_unit_at_once() {
        let mut term = term_with(20, 3, "one two/three (four)\r\nsecond line\r\nthird");
        let mut drag = selection_start(&mut term, 5, 0, SelectionKind::Word);
        assert_eq!(selection_text(&term).as_deref(), Some("two/three"));
        // Dragging grows it word by word.
        assert!(selection_update(&mut term, &mut drag, 15, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("two/three (four"));
        assert!(selection_update(&mut term, &mut drag, 1, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("one two/three"));

        let mut drag = selection_start(&mut term, 3, 1, SelectionKind::Line);
        assert_eq!(selection_text(&term).as_deref(), Some("second line\n"));
        assert_eq!(selected(&term)[1], "####################");
        assert!(selection_update(&mut term, &mut drag, 0, 2));
        assert_eq!(
            selection_text(&term).as_deref(),
            Some("second line\nthird\n")
        );
    }

    #[test]
    fn selected_tabs_are_copied_as_tabs() {
        let mut term = term_with(20, 2, "a\tb\tc");
        selection_start(&mut term, 0, 0, SelectionKind::Line);
        assert_eq!(selection_text(&term).as_deref(), Some("a\tb\tc\n"));
        // From inside the blanks of the first tab to the cell after the second.
        let mut drag = selection_start(&mut term, 4, 0, SelectionKind::Simple);
        assert!(selection_update(&mut term, &mut drag, 16, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("    b\tc"));
    }

    #[test]
    fn a_selection_takes_whole_wide_characters() {
        // Row one is "abc" and the placeholder for the 日 that had to move down a row.
        let mut term = term_with(4, 2, "abc日");
        let mut drag = selection_start(&mut term, 0, 0, SelectionKind::Simple);
        assert!(selection_update(&mut term, &mut drag, 2, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("abc"));
        // Ending on the placeholder means the character it stands for.
        assert!(selection_update(&mut term, &mut drag, 3, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("abc日"));
        selection_start(&mut term, 1, 0, SelectionKind::Line);
        assert_eq!(selection_text(&term).as_deref(), Some("abc日\n"));

        // Starting on the second half of a wide character takes all of it.
        let mut term = term_with(10, 1, "x日本y");
        let mut drag = selection_start(&mut term, 2, 0, SelectionKind::Simple);
        assert!(selection_update(&mut term, &mut drag, 3, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("日本"));
        assert!(selection_update(&mut term, &mut drag, 5, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("日本y"));
    }

    #[test]
    fn selecting_nothing_but_blanks_is_no_selection() {
        let mut term = term_with(10, 2, "ab");
        let mut drag = selection_start(&mut term, 4, 0, SelectionKind::Simple);
        assert!(selection_update(&mut term, &mut drag, 8, 0));
        assert_eq!(selection_text(&term), None);
        // Combining marks count as text even on a blank.
        feed(&mut term, "\x1b[1;6H \u{301}");
        assert_eq!(selection_text(&term).as_deref(), Some("  \u{301}"));
    }

    #[test]
    fn a_line_selection_follows_soft_wraps() {
        let mut term = term_with(5, 3, "abcdefgh\r\nxyz");
        selection_start(&mut term, 1, 1, SelectionKind::Line);
        assert_eq!(selection_text(&term).as_deref(), Some("abcdefgh\n"));
        assert_eq!(selected(&term), ["#####", "#####", "....."]);
    }

    #[test]
    fn the_selection_moves_with_the_text_and_can_still_be_extended() {
        let mut term = term_with(10, 3, "abcdefghij\r\nklmnopqrst");
        let mut drag = selection_start(&mut term, 2, 0, SelectionKind::Simple);
        assert!(selection_update(&mut term, &mut drag, 4, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("cde"));
        // Two more lines of output scroll everything up by one row.
        feed(&mut term, "\r\nthird\r\nfourth");
        assert_eq!(row_text(&term, 0), "klmnopqrst");
        assert_eq!(selection_text(&term).as_deref(), Some("cde"));
        // The drag goes on from where the text is now: row 0 is the second line.
        assert!(selection_update(&mut term, &mut drag, 1, 0));
        assert_eq!(selection_text(&term).as_deref(), Some("cdefghij\nkl"));
    }

    #[test]
    fn updating_a_selection_the_terminal_dropped_does_nothing() {
        let mut term = term_with(10, 2, "abcdefghij");
        let mut drag = selection_start(&mut term, 2, 0, SelectionKind::Simple);
        assert!(selection_update(&mut term, &mut drag, 4, 0));
        // Clearing the screen clears the selection with it.
        feed(&mut term, "\x1b[2J");
        assert_eq!(selection_text(&term), None);
        assert!(!selection_update(&mut term, &mut drag, 6, 0));
        assert_eq!(selection_text(&term), None);
        assert!(term.selection.is_none());
    }

    #[test]
    fn points_are_clamped_to_the_screen() {
        let term = term_with(10, 3, "");
        assert_eq!(point_at(&term, 0, 0), Point::new(Line(0), Column(0)));
        assert_eq!(point_at(&term, 9, 2), Point::new(Line(2), Column(9)));
        assert_eq!(point_at(&term, 500, 500), Point::new(Line(2), Column(9)));
    }

    #[test]
    fn modes_follow_the_child() {
        let mut term = term_with(10, 3, "");
        let modes = TermModes::from_mode(*term.mode());
        // A fresh terminal: nothing but alternate scroll, which is on by default.
        assert_eq!(
            modes,
            TermModes {
                alternate_scroll: true,
                ..TermModes::default()
            }
        );
        assert_eq!(modes.key_modes(), KeyModes::LEGACY);
        assert!(!modes.mouse_modes().reporting());

        feed(
            &mut term,
            "\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b[?2004h\x1b[?1h\x1b=\x1b[?1049h\x1b[>7u",
        );
        let modes = TermModes::from_mode(*term.mode());
        assert!(
            modes.mouse_reporting && !modes.mouse_drag && !modes.mouse_motion && modes.sgr_mouse
        );
        assert!(
            modes.focus_reporting
                && modes.bracketed_paste
                && modes.app_cursor
                && modes.app_keypad
                && modes.alt_screen
        );
        assert_eq!(modes.kitty_flags, 7);
        assert_eq!(
            modes.key_modes(),
            KeyModes {
                kitty_flags: 7,
                app_cursor: true,
                app_keypad: true
            }
        );
        assert_eq!(
            modes.mouse_modes(),
            MouseModes {
                click: true,
                drag: false,
                motion: false,
                sgr: true,
                utf8: false
            }
        );

        feed(&mut term, "\x1b[?1002h");
        assert_eq!(
            TermModes::from_mode(*term.mode()).mouse_modes(),
            MouseModes {
                click: false,
                drag: true,
                motion: false,
                sgr: true,
                utf8: false
            }
        );
        feed(&mut term, "\x1b[?1003h\x1b[?1005h");
        let modes = TermModes::from_mode(*term.mode());
        assert!(
            modes.mouse_reporting
                && modes.mouse_motion
                && !modes.mouse_drag
                && modes.utf8_mouse
                && !modes.sgr_mouse
        );

        // Leaving the alternate screen brings back the keyboard mode of the main one.
        feed(&mut term, "\x1b[?1049l\x1b[?1003l\x1b[?1007l\x1b>\x1b[?1l");
        let modes = TermModes::from_mode(*term.mode());
        assert!(
            !modes.alt_screen
                && !modes.mouse_reporting
                && !modes.alternate_scroll
                && !modes.app_keypad
                && !modes.app_cursor
        );
        assert_eq!(modes.kitty_flags, 0);
    }

    #[test]
    fn kitty_flags_are_pushed_set_and_popped() {
        let mut term = term_with(10, 3, "\x1b[>1u");
        let flags = |term: &Term<VoidListener>| TermModes::from_mode(*term.mode()).kitty_flags;
        assert_eq!(flags(&term), 1);
        feed(&mut term, "\x1b[>31u");
        assert_eq!(flags(&term), 31);
        feed(&mut term, "\x1b[=5;1u");
        assert_eq!(flags(&term), 5);
        feed(&mut term, "\x1b[=2;2u");
        assert_eq!(flags(&term), 7);
        feed(&mut term, "\x1b[=4;3u");
        assert_eq!(flags(&term), 3);
        feed(&mut term, "\x1b[<u");
        assert_eq!(flags(&term), 1);
        feed(&mut term, "\x1b[<5u");
        assert_eq!(flags(&term), 0);
    }

    #[test]
    fn links_are_mapped_to_cells() {
        let term = term_with(
            40,
            3,
            "see src/main.rs:10 and\r\nnothing here\r\n  Cargo.toml",
        );
        assert_eq!(
            links(&term),
            vec![
                link(0, 4, 18, "src/main.rs", Some(10), 0),
                link(2, 2, 12, "Cargo.toml", None, 1)
            ]
        );
    }

    #[test]
    fn wide_characters_before_a_link_shift_its_cells() {
        // Two wide characters take four cells but two characters of text.
        let term = term_with(40, 1, "日本 src/a.rs:3 語");
        assert_eq!(row_text(&term, 0), "日本 src/a.rs:3 語");
        assert_eq!(links(&term), vec![link(0, 5, 15, "src/a.rs", Some(3), 0)]);
        // A path with wide characters in it covers both halves of each.
        let term = term_with(40, 1, "x 文書/a.md y");
        assert_eq!(links(&term), vec![link(0, 2, 11, "文書/a.md", None, 0)]);
        // Combining marks do not take cells.
        let term = term_with(40, 1, "cafe\u{301}/a.rs:1 e\u{301} b.rs");
        let found = links(&term);
        assert_eq!((found[0].col_start, found[0].col_end), (0, 11));
        assert_eq!((found[1].col_start, found[1].col_end), (14, 18));
    }

    #[test]
    fn a_link_broken_by_the_edge_is_one_link_in_two_segments() {
        let term = term_with(10, 3, "ab src/main.rs:7 z");
        assert_eq!(row_text(&term, 0), "ab src/mai");
        assert_eq!(row_text(&term, 1), "n.rs:7 z");
        assert_eq!(
            links(&term),
            vec![
                link(0, 3, 10, "src/main.rs", Some(7), 0),
                link(1, 0, 6, "src/main.rs", Some(7), 0)
            ]
        );
        // A hard line break at the same place makes two separate things.
        let term = term_with(10, 3, "ab src/mai\r\nn.rs:7 z");
        assert_eq!(
            links(&term),
            vec![
                link(0, 3, 10, "src/mai", None, 0),
                link(1, 0, 6, "n.rs", Some(7), 1)
            ]
        );
    }

    #[test]
    fn a_link_over_three_rows() {
        let term = term_with(8, 4, "aaaa/bbbb/cccc/dddd/e.rs:12");
        let found = links(&term);
        assert_eq!(
            found,
            vec![
                link(0, 0, 8, "aaaa/bbbb/cccc/dddd/e.rs", Some(12), 0),
                link(1, 0, 8, "aaaa/bbbb/cccc/dddd/e.rs", Some(12), 0),
                link(2, 0, 8, "aaaa/bbbb/cccc/dddd/e.rs", Some(12), 0),
                link(3, 0, 3, "aaaa/bbbb/cccc/dddd/e.rs", Some(12), 0),
            ]
        );
        assert!(found[0].contains(7, 0) && !found[0].contains(8, 0) && !found[0].contains(0, 1));
    }

    #[test]
    fn a_wrapped_link_keeps_its_target_when_part_of_it_is_scrolled_away() {
        let mut term = term_with(10, 2, "src/long/path.rs:5\r\nnext\r\nlast");
        // The first row of the link is in the scrollback now.
        assert_eq!(screen_text(&term), "next\nlast");
        term.scroll_display(Scroll::Delta(1));
        assert_eq!(screen_text(&term), "ath.rs:5\nnext");
        assert_eq!(
            links(&term),
            vec![link(0, 0, 8, "src/long/path.rs", Some(5), 0)]
        );
        term.scroll_display(Scroll::Delta(1));
        assert_eq!(screen_text(&term), "src/long/p\nath.rs:5");
        assert_eq!(
            links(&term),
            vec![
                link(0, 0, 10, "src/long/path.rs", Some(5), 0),
                link(1, 0, 8, "src/long/path.rs", Some(5), 0)
            ]
        );
    }

    #[test]
    fn a_wide_character_pushed_to_the_next_row_does_not_break_a_link() {
        // Row one is "ab/文" plus a placeholder; the second 書 would not fit.
        let term = term_with(6, 2, "ab/文書.md");
        assert_eq!(row_text(&term, 0), "ab/文");
        assert_eq!(row_text(&term, 1), "書.md");
        assert_eq!(
            links(&term),
            vec![
                link(0, 0, 5, "ab/文書.md", None, 0),
                link(1, 0, 5, "ab/文書.md", None, 0)
            ]
        );
    }

    #[test]
    fn hyperlinks_the_program_made_win_over_guesses() {
        let term = term_with(40, 2, "\x1b]8;;https://example.com/docs\x07the docs\x1b]8;;\x07 and \x1b]8;;file:///abs/a%20b.rs#L4\x07a b.rs\x1b]8;;\x07 src/x.rs");
        let found = links(&term);
        assert_eq!(
            found,
            vec![
                ScreenLink {
                    row: 0,
                    col_start: 0,
                    col_end: 8,
                    target: LinkTarget::Url("https://example.com/docs".to_string()),
                    id: 1
                },
                ScreenLink {
                    row: 0,
                    col_start: 13,
                    col_end: 19,
                    target: LinkTarget::Path {
                        path: "/abs/a b.rs".to_string(),
                        line: Some(4),
                        col: None
                    },
                    id: 2
                },
                link(0, 20, 28, "src/x.rs", None, 0),
            ]
        );
    }

    #[test]
    fn a_hyperlink_over_text_that_looks_like_a_path_is_still_the_hyperlink() {
        let term = term_with(
            40,
            1,
            "\x1b]8;;https://example.com/blob/main.rs\x07src/main.rs:10\x1b]8;;\x07",
        );
        assert_eq!(
            links(&term),
            vec![ScreenLink {
                row: 0,
                col_start: 0,
                col_end: 14,
                target: LinkTarget::Url("https://example.com/blob/main.rs".to_string()),
                id: 0
            }]
        );
    }

    #[test]
    fn a_hyperlink_that_wraps_shares_one_id() {
        let term = term_with(
            6,
            3,
            "\x1b]8;id=x;https://example.com\x07click here\x1b]8;;\x07",
        );
        let found = links(&term);
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].row, found[0].col_start, found[0].col_end),
            (0, 0, 6)
        );
        assert_eq!(
            (found[1].row, found[1].col_start, found[1].col_end),
            (1, 0, 4)
        );
        assert_eq!(found[0].id, found[1].id);
        assert_eq!(
            found[0].target,
            LinkTarget::Url("https://example.com".to_string())
        );
    }

    #[test]
    fn pane_borders_keep_links_apart() {
        let term = term_with(
            40,
            2,
            "│src/a.rs:1    │src/b.rs:22   │\r\n├──────────────┼──────────────┤",
        );
        assert_eq!(
            links(&term),
            vec![
                link(0, 1, 11, "src/a.rs", Some(1), 0),
                link(0, 16, 27, "src/b.rs", Some(22), 1)
            ]
        );
    }

    #[test]
    fn a_view_the_emulator_scrolled_past_the_history_reads_as_the_top() {
        // Scrolled back to the oldest line, then a scroll region that starts below the top
        // row scrolls: alacritty moves the view up again although no history was added.
        let mut term = term_with(10, 4, "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
        term.scroll_display(Scroll::Top);
        let history = term.history_size();
        assert_eq!((history, view_offset(&term)), (2, 2));
        assert_eq!(row_text(&term, 0), "one");
        feed(&mut term, "\x1b[2;4r\x1b[4;1H\n\n\n\n\n");

        assert_eq!(term.history_size(), history);
        assert_eq!(view_offset(&term), history);
        assert_eq!(row_text(&term, 0), "one");
        assert_eq!(row_text(&term, 1), "two");
        assert_eq!(cells(&term).len(), 4);
        assert_eq!(cursor(&term, true), None);
        let _ = links(&term);
        assert_eq!(point_at(&term, 0, 0), Point::new(Line(-2), Column(0)));
        // Back at the live screen the region has scrolled its three rows away.
        term.scroll_display(Scroll::Bottom);
        assert_eq!(view_offset(&term), 0);
        assert_eq!(row_text(&term, 0), "three");
        assert_eq!(screen_text(&term), "three\n\n\n");
    }

    /// A small deterministic generator, so a failure can be reproduced from its seed.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, limit: u64) -> u64 {
            self.next() % limit
        }

        /// A piece of terminal output: text, wide and combining characters, and escape
        /// sequences both sensible and broken.
        fn output(&mut self) -> String {
            const PIECES: &[&str] = &[
                "a",
                "Z",
                " ",
                "/",
                ".",
                ":",
                "7",
                "src/a.rs:1",
                "日",
                "本",
                "e\u{301}",
                "\u{200d}",
                "🦀",
                "│",
                "\t",
                "\r",
                "\n",
                "\r\n",
                "\x08",
                "\x1b[",
                "\x1b]",
                "\x1b",
                ";",
                "m",
                "H",
                "J",
                "K",
                "L",
                "M",
                "P",
                "@",
                "S",
                "T",
                "X",
                "r",
                "h",
                "l",
                "?",
                "1049",
                "25",
                "2026",
                "1",
                "0",
                "99",
                "\x1b[2J",
                "\x1b[H",
                "\x1b[1;31m",
                "\x1b[0m",
                "\x1b[7m",
                "\x1b[4:3m",
                "\x1b[?1049h",
                "\x1b[?1049l",
                "\x1b[3;2H",
                "\x1b[2;4r",
                "\x1b[r",
                "\x1bM",
                "\x1bD",
                "\x1b7",
                "\x1b8",
                "\x1bc",
                "\x1b]8;;https://example.com\x07",
                "\x1b]8;;file:///tmp/x#L2\x07",
                "\x1b]8;;\x07",
                "\x1b]0;t\x07",
                "\x1b[5 q",
                "\x1b[?25l",
                "\x1b[?25h",
                "\x1b[>7u",
                "\x1b[<u",
                "\x1b[4h",
                "\x1b[4l",
                "\x1b[?7l",
                "\x1b[?7h",
                "\x1b[10b",
                "\x1b(0",
                "\x1b(B",
                "\x0e",
                "\x0f",
                "\x07",
                "\x18",
                "\u{9b}",
                "\u{feff}",
            ];
            let mut out = String::new();
            for _ in 0..self.below(40) {
                out.push_str(PIECES[self.below(PIECES.len() as u64) as usize]);
            }
            out
        }
    }

    /// Check the selected text against alacritty's own idea of it, wherever alacritty's
    /// function can be trusted: not where it reads the wrong row (a row ending in the
    /// placeholder of a wide character), not where it would index below column 0, and not
    /// across tabs, whose stops only alacritty knows when the child has moved them.
    fn compare_with_upstream(term: &Term<VoidListener>, ours: Option<String>, context: &str) {
        let Some(range) = term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(term))
        else {
            assert_eq!(ours, None, "{context}");
            return;
        };
        let grid = term.grid();
        let placeholder_at_end = grid[range.end.line][term.last_column()]
            .flags
            .contains(Flags::LEADING_WIDE_CHAR_SPACER);
        // A row that starts with the second half of a wide character, left behind by a
        // resize, makes alacritty step to the column before the first.
        let spacer_at_row_start = (range.start.line.0..=range.end.line.0).any(|line| {
            let first = if line == range.start.line.0 {
                range.start.column
            } else {
                Column(0)
            };
            first.0 == 0
                && grid[Line(line)][first]
                    .flags
                    .contains(Flags::WIDE_CHAR_SPACER)
        });
        if placeholder_at_end || spacer_at_row_start {
            return;
        }
        let theirs = term.selection_to_string().filter(|text| !text.is_empty());
        if theirs.as_deref().is_some_and(|text| text.contains('\t')) {
            return;
        }
        assert_eq!(ours, theirs, "{context}: {:?}", term.selection);
    }

    #[test]
    fn nothing_the_child_writes_can_make_reading_the_screen_panic() {
        for seed in 1..=300u64 {
            let mut noise = Noise(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut term = term_with(13, 5, "");
            let mut drag = None;
            for step in 0..120 {
                feed(&mut term, &noise.output());
                match noise.below(12) {
                    0 => term.resize(super::testing::Size(
                        2 + noise.below(30) as usize,
                        1 + noise.below(12) as usize,
                    )),
                    1 => term.scroll_display(Scroll::Delta(noise.below(9) as i32 - 4)),
                    2 => term.scroll_display(Scroll::Top),
                    3 => term.scroll_display(Scroll::Bottom),
                    4 => {
                        let kind = [
                            SelectionKind::Simple,
                            SelectionKind::Word,
                            SelectionKind::Line,
                        ][noise.below(3) as usize];
                        drag = Some(selection_start(
                            &mut term,
                            noise.below(40) as u16,
                            noise.below(20) as u16,
                            kind,
                        ));
                    }
                    5 | 6 => {
                        if let Some(drag) = drag.as_mut() {
                            selection_update(
                                &mut term,
                                drag,
                                noise.below(40) as u16,
                                noise.below(20) as u16,
                            );
                        }
                    }
                    _ => {}
                }

                let (cols, rows) = (term.columns(), term.screen_lines());
                let context = format!("seed {seed}, step {step}, {cols}x{rows}");
                let mut visited = 0;
                render(&term, &TermColors::default(), &mut |col, row, _| {
                    assert!(
                        usize::from(col) < cols && usize::from(row) < rows,
                        "{context}"
                    );
                    visited += 1;
                });
                assert_eq!(visited, cols * rows, "{context}");
                if let Some(cursor) = cursor(&term, true) {
                    assert!(
                        usize::from(cursor.col) < cols && usize::from(cursor.row) < rows,
                        "{context}"
                    );
                }
                let text = screen_text(&term);
                assert_eq!(text.split('\n').count(), rows, "{context}");
                for row in 0..rows as u16 + 2 {
                    let _ = row_text(&term, row);
                }
                for link in links(&term) {
                    assert!(link.col_start < link.col_end, "{context}: {link:?}");
                    assert!(
                        usize::from(link.col_end) <= cols && usize::from(link.row) < rows,
                        "{context}: {link:?}"
                    );
                }
                let selected = selection_text(&term);
                compare_with_upstream(&term, selected, &context);
                let _ = TermModes::from_mode(*term.mode());
            }
        }
    }

    #[test]
    fn an_empty_screen_has_no_links() {
        assert_eq!(links(&term_with(10, 3, "")), vec![]);
        assert_eq!(links(&term_with(2, 1, "")), vec![]);
    }
}
