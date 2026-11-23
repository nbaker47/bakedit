//! Reading the screen of a real child: cells, cursor, scrollback, resizing, selection and
//! links.

mod common;

use bakedit_core::{Color, Rgb};
use bakedit_term::{
    CursorShape, LinkTarget, Scroll, SelectionKind, TermConfig, Terminal, Underline,
};
use common::{sh, size_reporter, start};

/// An owned copy of what `render` shows for one cell.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    ch: char,
    marks: Vec<char>,
    fg: Color,
    bg: Color,
    bold: bool,
    italic: bool,
    underline: Underline,
    inverse: bool,
    wide: bool,
    wide_spacer: bool,
    selected: bool,
}

fn cells(term: &Terminal) -> Vec<Vec<Seen>> {
    let (cols, rows) = term.size();
    let mut grid: Vec<Vec<Seen>> = vec![Vec::new(); usize::from(rows)];
    term.render(&mut |col, row, cell| {
        assert_eq!(
            usize::from(col),
            grid[usize::from(row)].len(),
            "cells arrive left to right"
        );
        grid[usize::from(row)].push(Seen {
            ch: cell.ch,
            marks: cell.zerowidth.to_vec(),
            fg: cell.fg,
            bg: cell.bg,
            bold: cell.bold,
            italic: cell.italic,
            underline: cell.underline,
            inverse: cell.inverse,
            wide: cell.wide,
            wide_spacer: cell.wide_spacer,
            selected: cell.selected,
        });
    });
    assert!(
        grid.iter().all(|row| row.len() == usize::from(cols)),
        "every row is {cols} cells wide"
    );
    grid
}

#[test]
fn output_becomes_cells_with_colours_and_widths() {
    // red bold "red", a curly-underlined truecolour "curly", two wide characters, an e with
    // a combining acute accent, and "rev" in reverse video on a palette background.
    let script = "printf '\\033[1;31mred\\033[0m \\033[4:3m\\033[38;2;10;20;30mcurly\\033[0m \\346\\227\\245\\346\\234\\254 e\\314\\201 \\033[7;48;5;22mrev\\033[0m\\n'; exec cat";
    let session = start(sh(script));
    session.wait_for_text("rev");
    let term = &session.term;
    assert_eq!(term.row_text(0), "red curly 日本 e\u{301} rev");
    assert_eq!(
        term.screen_text().lines().next(),
        Some("red curly 日本 e\u{301} rev")
    );

    let grid = cells(term);
    let row = &grid[0];
    assert_eq!(
        (row[0].ch, row[0].fg, row[0].bold),
        ('r', Color::Indexed(1), true)
    );
    assert_eq!(
        (row[3].ch, row[3].fg, row[3].bold),
        (' ', Color::Default, false)
    );
    assert_eq!(
        (row[4].ch, row[4].fg, row[4].underline),
        ('c', Color::Rgb(Rgb::new(10, 20, 30)), Underline::Curly)
    );
    assert_eq!(row[9].underline, Underline::None);
    assert_eq!(
        (row[10].ch, row[10].wide, row[10].wide_spacer),
        ('日', true, false)
    );
    assert_eq!(
        (row[11].ch, row[11].wide, row[11].wide_spacer),
        (' ', false, true)
    );
    assert_eq!((row[12].ch, row[12].wide), ('本', true));
    assert!(row[13].wide_spacer);
    assert_eq!(
        (row[15].ch, row[15].marks.as_slice()),
        ('e', &['\u{301}'][..])
    );
    assert_eq!(
        (row[17].ch, row[17].inverse, row[17].bg),
        ('r', true, Color::Indexed(22))
    );
    assert_eq!(
        (row[20].ch, row[20].inverse, row[20].bg),
        (' ', false, Color::Default)
    );
    // The rest of the screen is blank.
    assert!(grid[5]
        .iter()
        .all(|cell| cell.ch == ' ' && cell.fg == Color::Default && cell.bg == Color::Default));

    // The cursor is on the line below, where cat waits.
    let cursor = term.cursor().expect("a visible cursor");
    assert_eq!(
        (cursor.col, cursor.row, cursor.shape, cursor.blinking),
        (0, 1, CursorShape::Block, false)
    );
}

#[test]
fn the_child_can_hide_and_reshape_the_cursor() {
    let session = start(sh(
        "printf 'ab\\033[6 q'; sleep 0.3; printf '\\033[?25l'; exec cat",
    ));
    session.wait_until("a steady bar cursor after ab", |term| {
        term.cursor().is_some_and(|cursor| {
            (cursor.col, cursor.row, cursor.shape, cursor.blinking)
                == (2, 0, CursorShape::Bar, false)
        })
    });
    session.wait_until("the cursor to be hidden", |term| term.cursor().is_none());
}

#[test]
fn scrollback_can_be_read_and_left_again() {
    let session = start(sh("seq 1 100; exec cat"));
    session.wait_for_text("100");
    let term = &session.term;
    // 100 lines and the empty one the cursor is on, less the 24 on screen.
    assert_eq!(term.history_len(), 77);
    assert_eq!(term.scroll_offset(), 0);
    assert_eq!(term.row_text(0), "78");
    assert_eq!(term.row_text(22), "100");
    assert_eq!(term.cursor().map(|cursor| cursor.row), Some(23));

    let generation = term.generation();
    assert!(term.scroll(Scroll::Top));
    assert!(term.generation() > generation);
    assert_eq!(term.scroll_offset(), 77);
    assert_eq!(term.row_text(0), "1");
    assert_eq!(term.row_text(23), "24");
    assert_eq!(term.cursor(), None);
    assert!(!term.scroll(Scroll::Top), "already there");
    assert!(!term.scroll(Scroll::Lines(5)));

    assert!(term.scroll(Scroll::Lines(-2)));
    assert_eq!(term.row_text(0), "3");
    assert!(term.scroll(Scroll::PageDown));
    assert_eq!(term.row_text(0), "27");
    assert!(term.scroll(Scroll::PageUp));
    assert_eq!(term.row_text(0), "3");
    assert!(term.scroll(Scroll::Lines(1)));
    assert_eq!(term.row_text(0), "2");

    assert!(term.scroll(Scroll::Bottom));
    assert_eq!(term.scroll_offset(), 0);
    assert_eq!(term.row_text(0), "78");
    assert!(term.cursor().is_some());
    assert!(!term.scroll(Scroll::Bottom));
    assert!(!term.scroll(Scroll::PageDown));
}

#[test]
fn the_scrollback_is_as_long_as_configured() {
    let session = start(TermConfig {
        scrollback: 10,
        rows: 5,
        ..sh("seq 1 100; exec cat")
    });
    session.wait_for_text("100");
    assert_eq!(session.term.history_len(), 10);
    assert!(session.term.scroll(Scroll::Top));
    assert_eq!(session.term.row_text(0), "87");

    let session = start(TermConfig {
        scrollback: 0,
        rows: 5,
        ..sh("seq 1 100; exec cat")
    });
    session.wait_for_text("100");
    assert_eq!(session.term.history_len(), 0);
    assert!(!session.term.scroll(Scroll::PageUp));
}

#[test]
fn resizing_reaches_the_child_and_the_grid() {
    let session = start(size_reporter());
    session.wait_for_text("SIZE 80x24 640x384");
    let term = &session.term;
    assert_eq!(term.size(), (80, 24));

    let generation = term.generation();
    term.resize(100, 30);
    assert_eq!(term.size(), (100, 30));
    assert!(term.generation() > generation);
    session.wait_for_text("SIZE 100x30 800x480");
    assert_eq!(cells(term).len(), 30);

    // The same size again is not a resize: no second report.
    term.resize(100, 30);
    term.set_cell_size(10, 20);
    session.wait_for_text("SIZE 100x30 1000x600");
    assert_eq!(session.term.screen_text().matches("SIZE 100x30").count(), 2);

    // Too small is raised to the smallest grid there is.
    term.resize(0, 0);
    assert_eq!(term.size(), (2, 1));
    assert_eq!(cells(term).len(), 1);
    term.resize(40, 10);
    session.wait_for_text("SIZE 40x10 400x200");
}

#[test]
fn a_narrower_terminal_rewraps_its_lines() {
    let session = start(TermConfig {
        cols: 20,
        rows: 6,
        ..sh("printf 'abcdefghijklmnopqrst\\nxyz\\n'; exec cat")
    });
    session.wait_for_text("xyz");
    let term = &session.term;
    assert_eq!(term.row_text(0), "abcdefghijklmnopqrst");
    assert_eq!(term.row_text(1), "xyz");
    assert_eq!(term.history_len(), 0);

    // One row of twenty becomes three of eight. The rows that no longer fit above the
    // cursor move into the scrollback, as they would have had they been printed that way.
    term.resize(8, 6);
    assert_eq!(term.size(), (8, 6));
    term.scroll(Scroll::Top);
    let rows: Vec<String> = (0..4).map(|row| term.row_text(row)).collect();
    assert_eq!(rows, ["abcdefgh", "ijklmnop", "qrst", "xyz"]);
    // The pieces are still one line: selecting it gives the whole line back.
    term.selection_start(2, 1, SelectionKind::Line);
    assert_eq!(
        term.selection_text().as_deref(),
        Some("abcdefghijklmnopqrst\n")
    );

    term.resize(20, 6);
    term.scroll(Scroll::Top);
    assert_eq!(term.row_text(0), "abcdefghijklmnopqrst");
    assert_eq!(term.row_text(1), "xyz");
}

#[test]
fn text_can_be_selected_and_copied() {
    let session = start(sh("printf 'hello world\\nsecond line\\n'; exec cat"));
    session.wait_for_text("second line");
    let term = &session.term;
    assert_eq!(term.selection_text(), None);

    let generation = term.generation();
    term.selection_start(6, 0, SelectionKind::Simple);
    assert_eq!(term.selection_text(), None, "a press alone selects nothing");
    term.selection_update(10, 0);
    assert_eq!(term.selection_text().as_deref(), Some("world"));
    assert!(term.generation() > generation);
    let grid = cells(term);
    let marked: String = grid[0][..14]
        .iter()
        .map(|cell| if cell.selected { '#' } else { '.' })
        .collect();
    assert_eq!(marked, "......#####...");
    assert!(grid[1].iter().all(|cell| !cell.selected));

    // Dragging back past the start keeps the start cell, and rows join with a newline.
    term.selection_update(2, 0);
    assert_eq!(term.selection_text().as_deref(), Some("llo w"));
    term.selection_update(5, 1);
    assert_eq!(term.selection_text().as_deref(), Some("world\nsecond"));

    term.selection_start(8, 1, SelectionKind::Word);
    assert_eq!(term.selection_text().as_deref(), Some("line"));
    term.selection_start(3, 0, SelectionKind::Line);
    assert_eq!(term.selection_text().as_deref(), Some("hello world\n"));

    term.selection_clear();
    assert_eq!(term.selection_text(), None);
    assert!(cells(term).iter().flatten().all(|cell| !cell.selected));
    // With nothing started, an update does nothing.
    term.selection_update(4, 0);
    assert_eq!(term.selection_text(), None);
}

#[test]
fn links_on_screen_are_found_by_cell() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

    let script = "printf '\\346\\227\\245 see src/main.rs:10:4 or and/or https://example.com/x.\\n'; exec cat";
    let session = start(sh(script));
    session.wait_for_text("example.com");
    let term = &session.term;
    assert_eq!(
        term.row_text(0),
        "日 see src/main.rs:10:4 or and/or https://example.com/x."
    );

    // The wide character takes two cells, so the path starts at cell 7.
    let link = term.link_at(7, 0).expect("a link under the path");
    assert_eq!((link.row, link.col_start, link.col_end), (0, 7, 23));
    assert_eq!(
        link.target,
        LinkTarget::Path {
            path: "src/main.rs".to_string(),
            line: Some(10),
            col: Some(4)
        }
    );
    assert_eq!(term.link_at(22, 0), Some(link.clone()));
    assert_eq!(term.link_at(6, 0), None);
    assert_eq!(term.link_at(23, 0), None);
    assert_eq!(term.link_at(7, 1), None);
    assert_eq!(term.link_at(500, 500), None);

    let links = term.visible_links();
    let targets: Vec<&LinkTarget> = links.iter().map(|link| &link.target).collect();
    assert_eq!(
        targets,
        [
            &LinkTarget::Path {
                path: "src/main.rs".to_string(),
                line: Some(10),
                col: Some(4)
            },
            &LinkTarget::Path {
                path: "and/or".to_string(),
                line: None,
                col: None
            },
            &LinkTarget::Url("https://example.com/x".to_string()),
        ]
    );
    // Only what exists resolves: the path does, the figure of speech does not.
    let bases = vec![root.clone()];
    let resolved = links[0].target.resolve(&bases).expect("the file exists");
    assert_eq!(
        (resolved.path, resolved.line, resolved.col),
        (root.join("src/main.rs"), Some(10), Some(4))
    );
    assert_eq!(links[1].target.resolve(&bases), None);
    assert_eq!(links[2].target.resolve(&bases), None);
}

#[test]
fn a_link_that_wraps_is_found_on_every_row_it_covers() {
    let session = start(TermConfig {
        cols: 12,
        rows: 6,
        ..sh("printf 'at crates/term/src/lib.rs:42 !\\n'; exec cat")
    });
    session.wait_for_text("!");
    let term = &session.term;
    assert_eq!(term.row_text(0), "at crates/te");
    assert_eq!(term.row_text(1), "rm/src/lib.r");
    assert_eq!(term.row_text(2), "s:42 !");

    let target = LinkTarget::Path {
        path: "crates/term/src/lib.rs".to_string(),
        line: Some(42),
        col: None,
    };
    let links = term.visible_links();
    let segments: Vec<(u16, u16, u16)> = links
        .iter()
        .map(|link| (link.row, link.col_start, link.col_end))
        .collect();
    assert_eq!(segments, [(0, 3, 12), (1, 0, 12), (2, 0, 4)]);
    assert!(links
        .iter()
        .all(|link| link.target == target && link.id == links[0].id));
    for (col, row) in [(3, 0), (11, 0), (0, 1), (11, 1), (0, 2), (3, 2)] {
        assert_eq!(
            term.link_at(col, row).map(|link| link.target),
            Some(target.clone()),
            "at {col},{row}"
        );
    }
    assert_eq!(term.link_at(2, 0), None);
    assert_eq!(term.link_at(4, 2), None);
}

#[test]
fn hyperlinks_written_by_the_program_are_links() {
    let script = "printf '\\033]8;;file:///tmp/a%%20b.txt#L3\\033\\\\the file\\033]8;;\\033\\\\ plain\\n'; exec cat";
    let session = start(sh(script));
    session.wait_for_text("plain");
    let term = &session.term;
    let link = term.link_at(4, 0).expect("the hyperlink");
    assert_eq!((link.col_start, link.col_end), (0, 8));
    assert_eq!(
        link.target,
        LinkTarget::Path {
            path: "/tmp/a b.txt".to_string(),
            line: Some(3),
            col: None
        }
    );
    assert_eq!(term.link_at(9, 0), None);
    assert_eq!(term.visible_links().len(), 1);
}

#[test]
fn the_generation_only_moves_when_the_screen_does() {
    let session = start(sh("printf 'ready\\n'; exec cat"));
    session.wait_for_text("ready");
    let term = &session.term;
    // Let the last wakeup for the output settle.
    let mut generation = term.generation();
    session.wait_until("the output to settle", |term| {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let now = term.generation();
        let settled = now == generation;
        generation = now;
        settled
    });

    // Reading changes nothing.
    let _ = (
        term.screen_text(),
        term.cursor(),
        term.visible_links(),
        term.modes(),
        term.selection_text(),
    );
    let _ = cells(term);
    assert_eq!(term.generation(), generation);
    assert!(!term.scroll(Scroll::PageUp), "no scrollback yet");
    term.selection_clear();
    term.resize(80, 24);
    assert_eq!(term.generation(), generation);

    // Output does.
    assert!(term.send_text("more\r"));
    session.wait_for_text("more");
    assert!(term.generation() > generation);
}
