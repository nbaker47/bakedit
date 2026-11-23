//! File names the way git on macOS spells them.
//!
//! A macOS file system keeps a file's name in the form it was created with, and
//! applications commonly create names decomposed: "u" followed by a combining diaeresis
//! instead of "ü". Git on macOS composes such names, both the ones it reads from disk and
//! the ones on its command line, and uses the composed form everywhere (that is its
//! `core.precomposeUnicode` setting, which it switches on in every repository it creates
//! on a Mac). It does not compose names that reach it on standard input, and what it
//! prints is always composed. A path that comes from the file system therefore has to be
//! composed the same way before it is handed to git or compared with git's output;
//! otherwise staging a file with an accent in its name quietly does nothing.

use std::path::PathBuf;

use unicode_normalization::char::{canonical_combining_class, compose};

/// `path` with every combining sequence composed. A path that is plain ASCII or not
/// UTF-8 is returned as it is.
pub(crate) fn precompose_path(path: PathBuf) -> PathBuf {
    match path.to_str() {
        Some(text) if !text.is_ascii() => PathBuf::from(precompose(text)),
        _ => path,
    }
}

/// Composes base characters with the combining marks (and Hangul jamo) that follow them.
///
/// This is the composing half of Unicode's NFC and nothing else: characters that are
/// already composed are never taken apart first. Full NFC would also replace a handful of
/// characters by look-alikes (some CJK compatibility ideographs, the ohm sign), which git
/// does not do, and a name spelled differently from git's is a different file to git.
fn precompose(text: &str) -> String {
    let mut composed: Vec<char> = Vec::with_capacity(text.len());
    // Where in `composed` the base character is that later marks may combine with.
    let mut base: Option<usize> = None;
    // The combining class of the last mark kept after the base; 0 when there is none.
    let mut last_class = 0;
    for c in text.chars() {
        let class = canonical_combining_class(c);
        if let Some(at) = base {
            // A mark only reaches the base past marks of a lower class, and a base
            // character only when it follows directly.
            let follows_directly = at + 1 == composed.len();
            let blocked = !follows_directly && last_class >= class;
            if !blocked {
                if let Some(joined) = compose(composed[at], c) {
                    composed[at] = joined;
                    continue;
                }
            }
        }
        if class == 0 {
            base = Some(composed.len());
            last_class = 0;
        } else {
            last_class = class;
        }
        composed.push(c);
    }
    composed.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn decomposed_letters_are_composed() {
        assert_eq!(precompose("u\u{308}.txt"), "\u{fc}.txt");
        assert_eq!(precompose("cafe\u{301}"), "caf\u{e9}");
        assert_eq!(precompose("A\u{30a}ngstro\u{308}m"), "\u{c5}ngstr\u{f6}m");
        // Kana with a voicing mark, and Hangul from its jamo.
        assert_eq!(precompose("\u{304b}\u{3099}"), "\u{304c}");
        assert_eq!(
            precompose("\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}"),
            "\u{d55c}\u{ae00}"
        );
    }

    #[test]
    fn several_marks_on_one_letter() {
        // Dot below, then circumflex: both find the letter.
        assert_eq!(precompose("a\u{323}\u{302}"), "\u{1ead}");
        // The second of two marks of the same class cannot pass the first.
        assert_eq!(precompose("a\u{302}\u{302}"), "\u{e2}\u{302}");
        // A mark that does not combine stays, and does not stop a later one.
        assert_eq!(precompose("a\u{321}\u{301}"), "\u{e1}\u{321}");
    }

    #[test]
    fn composed_and_plain_text_is_left_alone() {
        for text in [
            "",
            "plain ascii.txt",
            "\u{fc} \u{f1} caf\u{e9}",
            "\u{65e5}\u{672c}\u{8a9e}/\u{623f}\u{7522}.txt",
            "\u{d55c}\u{ae00}",
            // A mark with nothing before it to combine with.
            "\u{301}a",
            // Two letters that just stand next to each other.
            "ae",
        ] {
            assert_eq!(precompose(text), text, "{text:?}");
        }
    }

    #[test]
    fn characters_git_keeps_are_not_replaced_by_look_alikes() {
        // Full NFC would turn these into U+795E and U+03A9. Git leaves them, so we do.
        assert_eq!(precompose("\u{fa19}.txt"), "\u{fa19}.txt");
        assert_eq!(precompose("\u{2126}"), "\u{2126}");
    }

    #[test]
    fn paths_are_composed_only_when_they_need_it() {
        assert_eq!(
            precompose_path(PathBuf::from("dir/u\u{308}/e\u{301}.txt")),
            Path::new("dir/\u{fc}/\u{e9}.txt")
        );
        assert_eq!(
            precompose_path(PathBuf::from("dir/plain.txt")),
            Path::new("dir/plain.txt")
        );
        // Not UTF-8: nothing to compose, nothing changed.
        let raw = crate::paths::path_from_bytes(b"caf\xe9/u\xcc\x88");
        assert_eq!(precompose_path(raw.clone()), raw);
    }
}
