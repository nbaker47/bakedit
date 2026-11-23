//! Indentation: measuring it, building it, and guessing a file's style.

use crate::buffer::Buffer;

/// The leading whitespace of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineIndent {
    /// Number of leading space and tab characters.
    pub chars: usize,
    /// Display width of that whitespace, with tabs running to the next tab stop.
    pub width: usize,
}

/// How a file is indented, as guessed by [`detect_indent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndentStyle {
    /// Lines are indented with tab characters. How wide a tab looks is not in the file.
    Tabs,
    /// Lines are indented with this many spaces per level.
    Spaces(usize),
}

impl IndentStyle {
    /// The style as `(hard_tabs, width)`. For tabs the width is 0: keep the tab size you
    /// have.
    pub fn as_tuple(self) -> (bool, usize) {
        match self {
            IndentStyle::Tabs => (true, 0),
            IndentStyle::Spaces(width) => (false, width),
        }
    }
}

/// Measure the spaces and tabs at the start of line `line`. A line past the end has none.
pub fn indent_of_line(buffer: &Buffer, line: usize, tab_size: usize) -> LineIndent {
    let tab_size = tab_size.max(1);
    let mut indent = LineIndent::default();
    if line >= buffer.len_lines() {
        return indent;
    }
    for c in buffer.rope().chars_at(buffer.line_to_char(line)) {
        match c {
            ' ' => indent.width += 1,
            '\t' => indent.width += tab_size - indent.width % tab_size,
            _ => break,
        }
        indent.chars += 1;
    }
    indent
}

/// Whether line `line` has nothing but whitespace on it. A line past the end is blank.
pub fn is_blank_line(buffer: &Buffer, line: usize) -> bool {
    if line >= buffer.len_lines() {
        return true;
    }
    buffer
        .rope()
        .chars_at(buffer.line_to_char(line))
        .take_while(|&c| c != '\n')
        .all(char::is_whitespace)
}

/// The last character of line `line` that is not whitespace, if it has one.
pub fn last_non_blank(buffer: &Buffer, line: usize) -> Option<char> {
    if line >= buffer.len_lines() {
        return None;
    }
    let start = buffer.line_to_char(line);
    let mut chars = buffer.rope().chars_at(buffer.line_end(line));
    let mut position = buffer.line_end(line);
    while position > start {
        let c = chars.prev()?;
        if !c.is_whitespace() {
            return Some(c);
        }
        position -= 1;
    }
    None
}

/// Whitespace that is `width` columns wide: as many tabs as fit and spaces for the rest
/// with `hard_tabs`, only spaces otherwise.
pub fn indent_string(width: usize, hard_tabs: bool, tab_size: usize) -> String {
    let tab_size = tab_size.max(1);
    if hard_tabs {
        let mut out = "\t".repeat(width / tab_size);
        out.push_str(&" ".repeat(width % tab_size));
        out
    } else {
        " ".repeat(width)
    }
}

/// How many lines [`detect_indent`] looks at.
const DETECT_MAX_LINES: usize = 10_000;

/// The widest indent step [`detect_indent`] will report.
const DETECT_MAX_WIDTH: usize = 8;

/// Guess how the buffer is indented from its first 10,000 lines.
///
/// Lines that start with a tab vote for tabs, lines that start with two or more spaces vote
/// for spaces, and the step between the indents of neighbouring lines votes for a width
/// (the method VS Code uses). Returns `None` when no line is indented, so there is nothing
/// to go by and the caller should keep its defaults.
pub fn detect_indent(buffer: &Buffer) -> Option<IndentStyle> {
    let mut tab_lines = 0usize;
    let mut space_lines = 0usize;
    let mut steps = [0usize; DETECT_MAX_WIDTH + 1];
    let mut previous_spaces: Option<usize> = None;
    for line in 0..buffer.len_lines().min(DETECT_MAX_LINES) {
        let mut spaces = 0usize;
        let mut tabs = 0usize;
        let mut has_content = false;
        for c in buffer.rope().chars_at(buffer.line_to_char(line)) {
            match c {
                ' ' => spaces += 1,
                '\t' => tabs += 1,
                '\n' => break,
                _ => {
                    has_content = true;
                    break;
                }
            }
        }
        if !has_content {
            continue;
        }
        if tabs > 0 {
            tab_lines += 1;
            // A tab-indented line says nothing about space widths, and neither does the
            // step to or from it.
            previous_spaces = None;
            continue;
        }
        if spaces > 1 {
            space_lines += 1;
        }
        if let Some(previous) = previous_spaces {
            let step = spaces.abs_diff(previous);
            if (1..=DETECT_MAX_WIDTH).contains(&step) {
                steps[step] += 1;
            }
        }
        previous_spaces = Some(spaces);
    }
    if tab_lines == 0 && space_lines == 0 {
        return None;
    }
    if tab_lines > space_lines {
        return Some(IndentStyle::Tabs);
    }
    let mut width = 0usize;
    let mut best = 0usize;
    for candidate in [2, 4, 6, 8, 3, 5, 7] {
        if steps[candidate] > best {
            best = steps[candidate];
            width = candidate;
        }
    }
    // Code indented by two often has a few four-wide steps (continuation lines); let two
    // win when it is at least half as common.
    if width == 4 && steps[2] > 0 && steps[2] * 2 >= steps[4] {
        width = 2;
    }
    if width == 0 {
        return None;
    }
    Some(IndentStyle::Spaces(width))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measuring_indent() {
        let buffer = Buffer::from_str("none\n    four\n\ttab\n  \t mixed\n   \n\n");
        assert_eq!(
            indent_of_line(&buffer, 0, 4),
            LineIndent { chars: 0, width: 0 }
        );
        assert_eq!(
            indent_of_line(&buffer, 1, 4),
            LineIndent { chars: 4, width: 4 }
        );
        assert_eq!(
            indent_of_line(&buffer, 2, 4),
            LineIndent { chars: 1, width: 4 }
        );
        assert_eq!(
            indent_of_line(&buffer, 2, 8),
            LineIndent { chars: 1, width: 8 }
        );
        assert_eq!(
            indent_of_line(&buffer, 3, 4),
            LineIndent { chars: 4, width: 5 }
        );
        assert_eq!(
            indent_of_line(&buffer, 4, 4),
            LineIndent { chars: 3, width: 3 }
        );
        assert_eq!(indent_of_line(&buffer, 5, 4), LineIndent::default());
        assert_eq!(indent_of_line(&buffer, 99, 4), LineIndent::default());
        assert_eq!(
            indent_of_line(&buffer, 2, 0),
            LineIndent { chars: 1, width: 1 }
        );
    }

    #[test]
    fn blank_lines_and_last_characters() {
        let buffer = Buffer::from_str("code;  \n   \n\n\tx\n");
        assert!(!is_blank_line(&buffer, 0));
        assert!(is_blank_line(&buffer, 1));
        assert!(is_blank_line(&buffer, 2));
        assert!(!is_blank_line(&buffer, 3));
        assert!(is_blank_line(&buffer, 4));
        assert!(is_blank_line(&buffer, 99));
        assert_eq!(last_non_blank(&buffer, 0), Some(';'));
        assert_eq!(last_non_blank(&buffer, 1), None);
        assert_eq!(last_non_blank(&buffer, 2), None);
        assert_eq!(last_non_blank(&buffer, 3), Some('x'));
        assert_eq!(last_non_blank(&buffer, 99), None);
    }

    #[test]
    fn building_indent() {
        assert_eq!(indent_string(8, false, 4), "        ");
        assert_eq!(indent_string(8, true, 4), "\t\t");
        assert_eq!(indent_string(6, true, 4), "\t  ");
        assert_eq!(indent_string(0, true, 4), "");
        assert_eq!(indent_string(3, true, 0), "\t\t\t");
    }

    #[test]
    fn detects_spaces_of_each_width() {
        let four = "fn main() {\n    let x = 1;\n    if x > 0 {\n        go();\n    }\n}\n";
        assert_eq!(
            detect_indent(&Buffer::from_str(four)),
            Some(IndentStyle::Spaces(4))
        );
        let two = "a:\n  b:\n    c: 1\n    d: 2\n  e:\n    f: 3\n";
        assert_eq!(
            detect_indent(&Buffer::from_str(two)),
            Some(IndentStyle::Spaces(2))
        );
        let three = "x\n   y\n      z\n   w\n";
        assert_eq!(
            detect_indent(&Buffer::from_str(three)),
            Some(IndentStyle::Spaces(3))
        );
        let eight = "x\n        y\n                z\n";
        assert_eq!(
            detect_indent(&Buffer::from_str(eight)),
            Some(IndentStyle::Spaces(8))
        );
    }

    #[test]
    fn detects_tabs() {
        let tabs = "func main() {\n\tx := 1\n\tif x > 0 {\n\t\tgo()\n\t}\n}\n";
        assert_eq!(
            detect_indent(&Buffer::from_str(tabs)),
            Some(IndentStyle::Tabs)
        );
        assert_eq!(IndentStyle::Tabs.as_tuple(), (true, 0));
        assert_eq!(IndentStyle::Spaces(2).as_tuple(), (false, 2));
        // Mostly tabs with a little space alignment is still tabs.
        let mixed = "a {\n\tb\n\tc\n\t  d\n   e\n}\n";
        assert_eq!(
            detect_indent(&Buffer::from_str(mixed)),
            Some(IndentStyle::Tabs)
        );
    }

    #[test]
    fn nothing_to_go_by() {
        assert_eq!(detect_indent(&Buffer::new()), None);
        assert_eq!(
            detect_indent(&Buffer::from_str("flat\nfile\nno indent\n")),
            None
        );
        assert_eq!(
            detect_indent(&Buffer::from_str("   \n\n\t\n")),
            None,
            "blank lines do not vote"
        );
    }

    #[test]
    fn two_wins_over_occasional_four_wide_steps() {
        let text = "a\n  b\n      c\n  d\n    e\n  f\n      g\n";
        assert_eq!(
            detect_indent(&Buffer::from_str(text)),
            Some(IndentStyle::Spaces(2))
        );
    }
}
