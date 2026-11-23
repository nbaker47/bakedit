//! Line differences between two texts, computed in process for the editor gutter.
//!
//! Unlike everything else in this crate this does not run git: the "current" text is
//! usually an editor buffer that has not been saved, which git cannot see.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use similar::{capture_diff_slices_deadline, Algorithm, DiffTag};

/// How long [`line_hunks`] searches for a precise answer before it settles for a coarser
/// one.
pub const LINE_DIFF_TIMEOUT: Duration = Duration::from_millis(250);

/// What happened to a run of lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineHunkKind {
    /// Lines that are only in the current text.
    Added,
    /// Lines of the base text that were replaced by other lines.
    Modified,
    /// Lines of the base text that are gone.
    Deleted,
}

/// One changed run of lines. Line numbers are 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineHunk {
    /// Whether lines were added, replaced or deleted.
    pub kind: LineHunkKind,
    /// The first changed line in the current text. For a deletion: the line the deleted
    /// lines sat before, which is one past the last line when they were at the end.
    pub new_start: usize,
    /// How many lines of the current text the hunk covers; 0 for a deletion.
    pub new_lines: usize,
    /// The first line of the base text the hunk replaces. For an addition: the line of
    /// the base text the new lines were put before.
    pub old_start: usize,
    /// How many lines of the base text the hunk replaces; 0 for an addition.
    pub old_lines: usize,
}

/// The line ranges in which `current` differs from `base`, in order: what the editor
/// gutter marks. Gives up on precision after [`LINE_DIFF_TIMEOUT`]; see
/// [`line_hunks_with_deadline`] for what that means and for how lines are counted.
pub fn line_hunks(base: &str, current: &str) -> Vec<LineHunk> {
    line_hunks_with_deadline(base, current, Instant::now().checked_add(LINE_DIFF_TIMEOUT))
}

/// [`line_hunks`] with a deadline of the caller's choosing; `None` searches for as long
/// as it takes.
///
/// A line ends at `\n`, `\r\n` or a lone `\r`, and which of the three it is makes no
/// difference: a file checked in with CRLF line endings compares equal to an editor
/// buffer that holds the same text with `\n`. A last line without a line ending differs
/// from the same line with one, as it does for git. A text that ends with a line ending
/// has no further, empty line after it, so line `n` here is line `n` of an editor that
/// shows one.
///
/// The hunks are sorted, never touch or overlap, and always describe a correct edit:
/// replacing each hunk's base lines by its current lines turns `base` into `current`.
/// When the deadline passes before the search is done, the part that is not resolved yet
/// is reported as replaced wholesale. In the worst case that is a single
/// [`LineHunkKind::Modified`] hunk reaching from the first line that differs to the last
/// one that does: everything in between marked as modified.
pub fn line_hunks_with_deadline(
    base: &str,
    current: &str,
    deadline: Option<Instant>,
) -> Vec<LineHunk> {
    if base == current {
        return Vec::new();
    }
    let old = split_lines(base);
    let new = split_lines(current);
    // Most edits leave most of a file alone. Setting the common start and end aside
    // first keeps the work proportional to the part that changed.
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (old_ids, new_ids) = number_lines(
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    );

    let mut hunks: Vec<LineHunk> = Vec::new();
    for op in capture_diff_slices_deadline(Algorithm::Myers, &old_ids, &new_ids, deadline) {
        let (tag, old_range, new_range) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            continue;
        }
        let old_start = prefix + old_range.start;
        let new_start = prefix + new_range.start;
        match hunks.last_mut() {
            // Two changes with nothing unchanged between them are one hunk.
            Some(last)
                if last.old_start + last.old_lines == old_start
                    && last.new_start + last.new_lines == new_start =>
            {
                last.old_lines += old_range.len();
                last.new_lines += new_range.len();
                last.kind = kind_of(last.old_lines, last.new_lines);
            }
            _ => hunks.push(LineHunk {
                kind: kind_of(old_range.len(), new_range.len()),
                new_start,
                new_lines: new_range.len(),
                old_start,
                old_lines: old_range.len(),
            }),
        }
    }
    hunks
}

fn kind_of(old_lines: usize, new_lines: usize) -> LineHunkKind {
    match (old_lines, new_lines) {
        (0, _) => LineHunkKind::Added,
        (_, 0) => LineHunkKind::Deleted,
        _ => LineHunkKind::Modified,
    }
}

/// A line as it is compared: its text without the line ending, and whether it had one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Line<'a> {
    text: &'a str,
    terminated: bool,
}

/// Splits `text` at `\n`, `\r\n` and lone `\r`.
fn split_lines(text: &str) -> Vec<Line<'_>> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at < bytes.len() {
        let ending_len = match bytes[at] {
            b'\n' => 1,
            b'\r' if bytes.get(at + 1) == Some(&b'\n') => 2,
            b'\r' => 1,
            _ => {
                at += 1;
                continue;
            }
        };
        // Line endings are ASCII, so these are character boundaries.
        lines.push(Line {
            text: &text[start..at],
            terminated: true,
        });
        at += ending_len;
        start = at;
    }
    if start < bytes.len() {
        lines.push(Line {
            text: &text[start..],
            terminated: false,
        });
    }
    lines
}

/// Replaces every line by a number, the same number for equal lines, so the search
/// compares integers instead of strings.
fn number_lines<'a>(old: &[Line<'a>], new: &[Line<'a>]) -> (Vec<u32>, Vec<u32>) {
    let mut numbers: HashMap<Line<'a>, u32> = HashMap::new();
    let mut number_of = |line: &Line<'a>| {
        let next = u32::try_from(numbers.len()).unwrap_or(u32::MAX);
        *numbers.entry(*line).or_insert(next)
    };
    let old_ids = old.iter().map(&mut number_of).collect();
    let new_ids = new.iter().map(&mut number_of).collect();
    (old_ids, new_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Lcg;
    use LineHunkKind::{Added, Deleted, Modified};

    fn hunk(
        kind: LineHunkKind,
        new_start: usize,
        new_lines: usize,
        old_start: usize,
        old_lines: usize,
    ) -> LineHunk {
        LineHunk {
            kind,
            new_start,
            new_lines,
            old_start,
            old_lines,
        }
    }

    /// Checks everything the documentation promises about `hunks` and returns them.
    fn checked(base: &str, current: &str, hunks: Vec<LineHunk>) -> Vec<LineHunk> {
        let old = split_lines(base);
        let new = split_lines(current);
        let (mut old_at, mut new_at) = (0, 0);
        for (index, hunk) in hunks.iter().enumerate() {
            assert!(hunk.old_lines + hunk.new_lines > 0, "empty hunk {hunk:?}");
            assert_eq!(hunk.kind, kind_of(hunk.old_lines, hunk.new_lines));
            // Sorted, and separated from the previous hunk by at least one line.
            assert!(hunk.old_start >= old_at && hunk.new_start >= new_at);
            if index > 0 {
                assert!(hunk.old_start > old_at, "hunks touch: {hunks:?}");
            }
            // The lines between the hunks are the same in both texts.
            let unchanged = hunk.old_start - old_at;
            assert_eq!(unchanged, hunk.new_start - new_at, "{hunks:?}");
            assert_eq!(
                old[old_at..hunk.old_start],
                new[new_at..hunk.new_start],
                "{hunks:?}"
            );
            old_at = hunk.old_start + hunk.old_lines;
            new_at = hunk.new_start + hunk.new_lines;
            assert!(old_at <= old.len() && new_at <= new.len(), "{hunks:?}");
        }
        assert_eq!(old[old_at..], new[new_at..], "{hunks:?}");
        hunks
    }

    fn diff(base: &str, current: &str) -> Vec<LineHunk> {
        checked(base, current, line_hunks(base, current))
    }

    #[test]
    fn equal_texts_have_no_hunks() {
        assert!(diff("", "").is_empty());
        assert!(diff("a\nb\n", "a\nb\n").is_empty());
        assert!(diff("no newline", "no newline").is_empty());
    }

    #[test]
    fn added_lines() {
        assert_eq!(diff("a\nc\n", "a\nb\nc\n"), [hunk(Added, 1, 1, 1, 0)]);
        assert_eq!(diff("b\n", "a1\na2\nb\n"), [hunk(Added, 0, 2, 0, 0)]);
        assert_eq!(diff("a\n", "a\nb\nc\n"), [hunk(Added, 1, 2, 1, 0)]);
        assert_eq!(diff("", "a\nb\n"), [hunk(Added, 0, 2, 0, 0)]);
        assert_eq!(diff("", "a"), [hunk(Added, 0, 1, 0, 0)]);
    }

    #[test]
    fn deleted_lines_sit_before_the_line_that_follows_them() {
        assert_eq!(diff("a\nb\nc\n", "a\nc\n"), [hunk(Deleted, 1, 0, 1, 1)]);
        assert_eq!(diff("a\nb\nc\n", "c\n"), [hunk(Deleted, 0, 0, 0, 2)]);
        // At the end there is no following line: one past the last.
        assert_eq!(diff("a\nb\nc\n", "a\n"), [hunk(Deleted, 1, 0, 1, 2)]);
        assert_eq!(diff("a\nb\n", ""), [hunk(Deleted, 0, 0, 0, 2)]);
    }

    #[test]
    fn modified_lines() {
        assert_eq!(diff("a\nb\nc\n", "a\nB\nc\n"), [hunk(Modified, 1, 1, 1, 1)]);
        assert_eq!(
            diff("a\nb\nc\nd\n", "a\nX\nY\nZ\nd\n"),
            [hunk(Modified, 1, 3, 1, 2)]
        );
        assert_eq!(diff("only\n", "other\n"), [hunk(Modified, 0, 1, 0, 1)]);
    }

    #[test]
    fn several_hunks_in_one_file() {
        let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let current = "0\n1\n2\nthree\n4\n5\n7\n8\n9\n10\n";
        assert_eq!(
            diff(base, current),
            [
                hunk(Added, 0, 1, 0, 0),
                hunk(Modified, 3, 1, 2, 1),
                hunk(Deleted, 6, 0, 5, 1),
                hunk(Added, 9, 1, 9, 0),
            ]
        );
    }

    #[test]
    fn a_missing_final_line_ending_is_a_change_to_the_last_line() {
        assert_eq!(diff("a\nb\n", "a\nb"), [hunk(Modified, 1, 1, 1, 1)]);
        assert_eq!(diff("a\nb", "a\nb\n"), [hunk(Modified, 1, 1, 1, 1)]);
        // Appending after a last line without ending changes that line too.
        assert_eq!(diff("a\nb", "a\nb\nc\n"), [hunk(Modified, 1, 2, 1, 1)]);
        assert_eq!(diff("a", ""), [hunk(Deleted, 0, 0, 0, 1)]);
    }

    #[test]
    fn the_kind_of_line_ending_does_not_matter() {
        assert!(diff("a\r\nb\r\n", "a\nb\n").is_empty());
        assert!(diff("a\rb\r", "a\nb\n").is_empty());
        assert!(diff("a\r\nb\nc\r", "a\nb\r\nc\n").is_empty());
        // Real changes are still found, at the same lines.
        assert_eq!(
            diff("a\r\nb\r\nc\r\n", "a\nB\nc\nd\n"),
            [hunk(Modified, 1, 1, 1, 1), hunk(Added, 3, 1, 3, 0)]
        );
        // A carriage return and a line feed that do not belong together are two endings.
        assert_eq!(diff("a\n\rb\n", "a\nb\n"), [hunk(Deleted, 1, 0, 1, 1)]);
    }

    #[test]
    fn blank_lines_are_lines() {
        assert_eq!(diff("a\nb\n", "a\n\nb\n"), [hunk(Added, 1, 1, 1, 0)]);
        assert_eq!(diff("\n\n\n", "\n\n"), [hunk(Deleted, 2, 0, 2, 1)]);
        assert_eq!(diff("a\n", "a\n\n"), [hunk(Added, 1, 1, 1, 0)]);
    }

    #[test]
    fn text_beyond_ascii_is_compared_whole() {
        assert!(diff(
            "gr\u{fc}n\n\u{65e5}\u{672c}\n",
            "gr\u{fc}n\n\u{65e5}\u{672c}\n"
        )
        .is_empty());
        assert_eq!(
            diff(
                "gr\u{fc}n\n\u{65e5}\u{672c}\n",
                "gr\u{fc}n\n\u{65e5}\u{672c}\u{8a9e}\n"
            ),
            [hunk(Modified, 1, 1, 1, 1)]
        );
    }

    #[test]
    fn splitting_lines() {
        let texts = |text: &'static str| -> Vec<(&'static str, bool)> {
            split_lines(text)
                .into_iter()
                .map(|line| (line.text, line.terminated))
                .collect()
        };
        assert!(texts("").is_empty());
        assert_eq!(texts("a"), [("a", false)]);
        assert_eq!(texts("a\n"), [("a", true)]);
        assert_eq!(texts("a\nb"), [("a", true), ("b", false)]);
        assert_eq!(
            texts("a\r\nb\rc\n"),
            [("a", true), ("b", true), ("c", true)]
        );
        assert_eq!(texts("\n\r\n\r"), [("", true), ("", true), ("", true)]);
        assert_eq!(texts("\r\r\n"), [("", true), ("", true)]);
    }

    /// A short text over few distinct lines, so there are many ways to line two of them
    /// up, with every kind of line ending.
    fn random_text(random: &mut Lcg) -> String {
        let mut text = String::new();
        for _ in 0..random.next(12) {
            text.push_str(["alpha", "beta", "gamma", "", "delta"][random.next(5) as usize]);
            text.push_str(["\n", "\n", "\n", "\r\n", "\r"][random.next(5) as usize]);
        }
        if random.next(4) == 0 {
            text.push_str("unterminated");
        }
        text
    }

    #[test]
    fn hunks_are_a_correct_edit_for_many_random_texts() {
        let mut random = Lcg(42);
        for _ in 0..3000 {
            let base = random_text(&mut random);
            let current = random_text(&mut random);
            let hunks = diff(&base, &current);
            // The same promises hold when there is no time at all.
            let past = Instant::now().checked_sub(Duration::from_secs(1));
            let rushed = line_hunks_with_deadline(&base, &current, past.or(Some(Instant::now())));
            checked(&base, &current, rushed);
            // And without a deadline the result is the same as with a comfortable one.
            assert_eq!(line_hunks_with_deadline(&base, &current, None), hunks);
        }
    }

    #[test]
    fn an_expired_deadline_marks_everything_between_the_common_ends_as_modified() {
        // Long enough that the search is not trivial, with no line in common in the
        // middle part.
        let mut base = String::from("same start\n");
        let mut current = String::from("same start\n");
        for line in 0..2000 {
            base.push_str(&format!("old {} {}\n", line % 7, line % 13));
            current.push_str(&format!("new {} {}\n", line % 5, line % 11));
        }
        base.push_str("same end\n");
        current.push_str("same end\n");

        let Some(past) = Instant::now().checked_sub(Duration::from_secs(1)) else {
            return;
        };
        let hunks = checked(
            &base,
            &current,
            line_hunks_with_deadline(&base, &current, Some(past)),
        );
        assert_eq!(hunks, [hunk(Modified, 1, 2000, 1, 2000)]);
    }

    #[test]
    fn an_expired_deadline_still_gives_a_correct_edit_for_similar_texts() {
        let lines: Vec<String> = (0..5000)
            .map(|line| format!("line {}\n", line % 50))
            .collect();
        let base: String = lines.concat();
        let mut changed = lines.clone();
        for line in (0..changed.len()).step_by(7) {
            changed[line] = format!("changed {line}\n");
        }
        changed.remove(1234);
        changed.insert(3000, "inserted\n".to_string());
        let current: String = changed.concat();

        let Some(past) = Instant::now().checked_sub(Duration::from_secs(1)) else {
            return;
        };
        let rushed = checked(
            &base,
            &current,
            line_hunks_with_deadline(&base, &current, Some(past)),
        );
        // With time the same texts give a much finer answer.
        let precise = diff(&base, &current);
        assert!(!rushed.is_empty() && rushed.len() < precise.len());
    }

    #[test]
    fn a_hard_comparison_is_cut_short_by_the_deadline() {
        // Two long texts over three distinct lines: countless ways to line them up.
        // Searching them to the end takes seconds.
        let mut random = Lcg(7);
        let mut text = |lines: usize| -> String {
            (0..lines)
                .map(|_| ["a\n", "b\n", "c\n"][random.next(3) as usize])
                .collect()
        };
        let base = text(400_000);
        let current = text(400_000);

        let started = Instant::now();
        let hunks = line_hunks(&base, &current);
        let elapsed = started.elapsed();
        checked(&base, &current, hunks);
        // The deadline is a quarter of a second. A busy machine gets a lot more than
        // that before this counts as a freeze.
        assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    }

    #[test]
    fn a_large_file_with_a_small_edit_is_fast() {
        let base: String = (0..200_000).map(|line| format!("line {line}\n")).collect();
        let current = base.replacen("line 150000\n", "line changed\n", 1);
        let started = Instant::now();
        let hunks = diff(&base, &current);
        assert_eq!(hunks, [hunk(Modified, 150_000, 1, 150_000, 1)]);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
