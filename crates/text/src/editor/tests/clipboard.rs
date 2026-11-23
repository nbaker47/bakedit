//! Copy, cut, paste and the kill ring.

use super::{apply_all, check, editor, state};
use crate::command::{Clipboard, ClipboardPiece, Command};

fn piece(text: &str, linewise: bool) -> ClipboardPiece {
    ClipboardPiece {
        text: text.to_string(),
        linewise,
    }
}

fn paste(text: &str) -> Command {
    Command::Paste(Clipboard::plain(text))
}

/// Run `command` and return what it put on the clipboard.
fn clipboard_of(marked: &str, command: Command) -> (Clipboard, String) {
    let mut e = editor(marked);
    let outcome = e.apply(command);
    (
        outcome.clipboard.expect("something for the clipboard"),
        state(&e),
    )
}

#[test]
fn copy_takes_the_selection_and_changes_nothing() {
    let (clip, after) = clipboard_of("a«bcˇ»d", Command::Copy);
    assert_eq!(
        clip,
        Clipboard {
            text: "bc".to_string(),
            linewise: false,
            pieces: vec![piece("bc", false)]
        }
    );
    assert_eq!(after, "a«bcˇ»d");
    let mut e = editor("a«bcˇ»d");
    let outcome = e.apply(Command::Copy);
    assert!(!outcome.text_changed() && !outcome.selections_changed && !outcome.is_noop());
}

#[test]
fn copy_at_a_cursor_takes_the_whole_line() {
    let (clip, after) = clipboard_of("one\ntˇwo\nthree", Command::Copy);
    assert_eq!(
        clip,
        Clipboard {
            text: "two\n".to_string(),
            linewise: true,
            pieces: vec![piece("two\n", true)]
        }
    );
    assert_eq!(after, "one\ntˇwo\nthree");
    // The last line gets the line break it does not have.
    let (clip, _) = clipboard_of("one\ntwˇo", Command::Copy);
    assert_eq!(clip.text, "two\n");
    assert!(clip.linewise);
    let (clip, _) = clipboard_of("ˇ", Command::Copy);
    assert_eq!(clip.text, "\n");
}

#[test]
fn copy_from_several_selections() {
    let (clip, _) = clipboard_of("«oneˇ» two «threeˇ»", Command::Copy);
    assert_eq!(clip.text, "one\nthree");
    assert_eq!(
        clip.pieces,
        vec![piece("one", false), piece("three", false)]
    );
    assert!(!clip.linewise);

    // Cursors give their lines, each line once.
    let (clip, _) = clipboard_of("aˇaˇ\nbb\ncˇc", Command::Copy);
    assert_eq!(clip.text, "aa\ncc\n");
    assert_eq!(clip.pieces, vec![piece("aa\n", true), piece("cc\n", true)]);
    assert!(clip.linewise);

    // A mix is not line-wise as a whole, but each piece knows what it is.
    let (clip, _) = clipboard_of("aˇa\n«bbˇ»", Command::Copy);
    assert_eq!(clip.text, "aa\nbb");
    assert_eq!(clip.pieces, vec![piece("aa\n", true), piece("bb", false)]);
    assert!(!clip.linewise);
}

#[test]
fn cut_removes_what_it_copies() {
    let (clip, after) = clipboard_of("a«bcˇ»d", Command::Cut);
    assert_eq!(clip.text, "bc");
    assert_eq!(after, "aˇd");

    let (clip, after) = clipboard_of("one\ntˇwo\nthree", Command::Cut);
    assert_eq!(clip.text, "two\n");
    assert!(clip.linewise);
    assert_eq!(after, "one\nˇthree");

    let (clip, after) = clipboard_of("one\ntˇwo", Command::Cut);
    assert_eq!(clip.text, "two\n");
    assert_eq!(after, "one\nˇ");

    let (clip, after) = clipboard_of("aˇaˇ\nbb", Command::Cut);
    assert_eq!(clip.pieces.len(), 1, "two cursors on a line cut it once");
    assert_eq!(after, "ˇbb");

    let (clip, after) = clipboard_of("«oneˇ» two «threeˇ»", Command::Cut);
    assert_eq!(clip.text, "one\nthree");
    assert_eq!(after, "ˇ two ˇ");
}

#[test]
fn paste_plain_text() {
    check("abˇc", paste("XY"), "abXYˇc");
    check("a«bcˇ»d", paste("XY"), "aXYˇd");
    check("ˇ", paste("one\ntwo"), "one\ntwoˇ");
    check("aˇ\nbˇ", paste("-"), "a-ˇ\nb-ˇ");
    check("abˇ", paste("x\r\ny"), "abx\nyˇ");
    check("abˇc", paste(""), "abˇc");
    assert!(editor("abˇc").apply(paste("")).is_noop());
    // Nothing is re-indented.
    check("    ˇ", paste("if x {\n  y\n}"), "    if x {\n  y\n}ˇ");
}

#[test]
fn paste_a_line_above_the_cursor_line() {
    let line = Clipboard::from_pieces(vec![piece("new\n", true)]);
    check("one\ntˇwo", Command::Paste(line.clone()), "one\nnew\ntˇwo");
    check("ˇone", Command::Paste(line.clone()), "new\nˇone");
    check("one\ntwoˇ", Command::Paste(line.clone()), "one\nnew\ntwoˇ");
    // Onto a selection it is ordinary text.
    check("o«nˇ»e", Command::Paste(line.clone()), "onew\nˇe");
    // Every cursor gets the line, even two on the same line.
    check("aˇ\nbˇ", Command::Paste(line.clone()), "new\naˇ\nnew\nbˇ");
    check("aˇbˇ", Command::Paste(line), "new\nnew\naˇbˇ");
    // A line-wise clipboard from elsewhere may lack its final line break.
    let bare = Clipboard {
        text: "new".to_string(),
        linewise: true,
        pieces: Vec::new(),
    };
    check("one\ntˇwo", Command::Paste(bare), "one\nnew\ntˇwo");
}

#[test]
fn paste_one_piece_per_cursor() {
    let two = Clipboard::from_pieces(vec![piece("A", false), piece("B", false)]);
    check("xˇ yˇ", Command::Paste(two.clone()), "xAˇ yBˇ");
    check("«xˇ» «yˇ»", Command::Paste(two.clone()), "Aˇ Bˇ");
    // With a different number of cursors everyone gets the whole text.
    check("xˇ", Command::Paste(two.clone()), "xA\nBˇ");
    check("xˇ yˇ zˇ", Command::Paste(two), "xA\nBˇ yA\nBˇ zA\nBˇ");

    let mixed = Clipboard::from_pieces(vec![piece("L\n", true), piece("w", false)]);
    check("aˇb\ncˇd", Command::Paste(mixed), "L\naˇb\ncwˇd");
}

#[test]
fn cut_and_paste_round_trip() {
    let mut e = editor("one\ntˇwo\nthree");
    let clip = e.apply(Command::Cut).clipboard.unwrap();
    assert_eq!(state(&e), "one\nˇthree");
    apply_all(&mut e, [Command::Paste(clip.clone())]);
    assert_eq!(state(&e), "one\ntwo\nˇthree");

    // Copy three words with three cursors and paste them over three others.
    let mut e = editor("«aˇ» «bˇ» «cˇ»\n1 2 3");
    let clip = e.apply(Command::Copy).clipboard.unwrap();
    e.set_selections(crate::Selections::from_ranges([6..7, 8..9, 10..11]));
    apply_all(&mut e, [Command::Paste(clip)]);
    assert_eq!(state(&e), "a b c\naˇ bˇ cˇ");
}

#[test]
fn paste_is_one_undo_step() {
    let mut e = editor("aˇ\nbˇ");
    apply_all(&mut e, [paste("XYZ"), Command::Undo]);
    assert_eq!(state(&e), "aˇ\nbˇ");
    apply_all(&mut e, [Command::Redo]);
    assert_eq!(state(&e), "aXYZˇ\nbXYZˇ");
}

#[test]
fn kill_ring() {
    let mut e = editor("one tˇwo three\nnext");
    let outcome = e.apply(Command::KillRingCut);
    assert!(
        outcome.clipboard.is_none(),
        "the kill ring is not the clipboard"
    );
    assert_eq!(state(&e), "one tˇ\nnext");
    assert_eq!(
        e.kill_ring().map(|clip| clip.text.as_str()),
        Some("wo three")
    );

    // At the end of the line the line break is taken.
    apply_all(&mut e, [Command::KillRingCut]);
    assert_eq!(state(&e), "one tˇnext");
    assert_eq!(e.kill_ring().map(|clip| clip.text.as_str()), Some("\n"));

    apply_all(&mut e, [Command::KillRingYank, Command::KillRingYank]);
    assert_eq!(state(&e), "one t\n\nˇnext");
}

#[test]
fn kill_ring_with_selections_and_nothing_to_cut() {
    let mut e = editor("a«bcˇ»d");
    apply_all(&mut e, [Command::KillRingCut]);
    assert_eq!(state(&e), "aˇd");
    apply_all(
        &mut e,
        [Command::Move(crate::Motion::End), Command::KillRingYank],
    );
    assert_eq!(state(&e), "adbcˇ");

    let mut e = editor("abcˇ");
    assert!(
        e.apply(Command::KillRingYank).is_noop(),
        "nothing was ever cut"
    );
    assert!(
        e.apply(Command::KillRingCut).is_noop(),
        "nothing after the cursor"
    );
    assert_eq!(e.kill_ring(), None);

    e.set_kill_ring(Some(Clipboard::plain("!")));
    apply_all(&mut e, [Command::KillRingYank]);
    assert_eq!(state(&e), "abc!ˇ");
}
