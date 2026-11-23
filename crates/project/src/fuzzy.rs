//! The file finder's query syntax and its path-aware fuzzy matching.
//!
//! [`parse_query`] splits what the user typed into a path part and an optional position
//! (`main.rs:12:3`). The matching itself is built on `nucleo-matcher` and lives behind
//! [`crate::FileIndex::search`]; this module holds the scoring rules so they can be read
//! and tested in one place.
//!
//! # How a path is scored
//!
//! The query is split at whitespace into words; every word has to match, in any order.
//! Matching is smart-case per word: a word with an upper-case letter is case-sensitive.
//!
//! * A word without `/` is tried against the **file name** first. If it matches there,
//!   the path gets [`NAME_BONUS`] on top of the match score, so any match in a file name
//!   ranks above any match that needs the directory part. Typing a name exactly, or its
//!   stem, or its beginning, adds a little more.
//! * Otherwise the word is matched against the whole path, where a letter right after a
//!   `/` counts as the start of a word.
//! * A word with a `/` is matched against the whole path; it earns [`NAME_BONUS`] when
//!   what follows its last `/` also matches the file name (`core/keys` → `core/src/keys.rs`).
//!
//! Equal scores are ordered by: recently opened files first (most recent first), then
//! the shorter path, then alphabetically.

use std::cmp::Ordering;
use std::path::PathBuf;

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Added to a path's score for every query word that matches inside the file name.
/// Larger than any score `nucleo-matcher` can give a single word.
pub const NAME_BONUS: u32 = 1 << 17;

/// Extra for a word that is the whole file name (`Cargo.toml`).
const EXACT_NAME_BONUS: u32 = 64;
/// Extra for a word that is the file name without its extension (`main` for `main.rs`)
/// or without its leading dot (`gitignore` for `.gitignore`).
const EXACT_STEM_BONUS: u32 = 48;
/// Extra for a word the file name starts with.
const NAME_PREFIX_BONUS: u32 = 8;

/// What the user typed into the file finder, taken apart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileQuery {
    /// The part to match against paths. Pass it to [`crate::FileIndex::search`].
    pub path_query: String,
    /// The 1-based line after the first `:`, as typed (`0` is not corrected).
    pub line: Option<u32>,
    /// The 1-based column after the second `:`, as typed.
    pub col: Option<u32>,
}

/// Split a file finder query into its path and position, as Zed does: `file`,
/// `file:12` and `file:12:3`.
///
/// Surrounding whitespace and trailing colons are dropped, so `file:` and `file:12:`
/// still find `file` while the user is typing. A suffix only counts as a position when
/// it is all digits and fits a `u32`; `file:abc` is a path. `:12` gives an empty path
/// with line 12.
pub fn parse_query(text: &str) -> FileQuery {
    let body = text.trim().trim_end_matches(':');
    let Some((before_last, last)) = split_trailing_number(body) else {
        return FileQuery {
            path_query: body.to_string(),
            line: None,
            col: None,
        };
    };
    match split_trailing_number(before_last) {
        Some((path, line)) => FileQuery {
            path_query: path.trim_end_matches(':').to_string(),
            line: Some(line),
            col: Some(last),
        },
        None => FileQuery {
            path_query: before_last.trim_end_matches(':').to_string(),
            line: Some(last),
            col: None,
        },
    }
}

/// Split `text:123` into `("text", 123)`.
fn split_trailing_number(text: &str) -> Option<(&str, u32)> {
    let (head, digits) = text.rsplit_once(':')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((head, digits.parse().ok()?))
}

/// One result of a file finder search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatch {
    /// The file, relative to the project root.
    pub path: PathBuf,
    /// Higher is better. Only comparable between results of the same query. `0` for the
    /// recent files an empty query returns.
    pub score: u32,
    /// The `char` positions in the displayed path (the relative path with `/`
    /// separators) that matched the query, ascending, for highlighting.
    pub indices: Vec<usize>,
}

/// One whitespace-separated word of the query, prepared for matching.
struct Word {
    /// The word as typed.
    text: String,
    atom: Atom,
    /// For a word with a `/`: what follows its last `/`, if anything.
    name_part: Option<Atom>,
    has_slash: bool,
    case_sensitive: bool,
}

/// A parsed file finder query: the words that all have to match.
pub(crate) struct Query {
    words: Vec<Word>,
}

impl Query {
    /// Prepare `text` for matching. A leading `./` on a word is dropped.
    pub(crate) fn new(text: &str) -> Query {
        let words = text
            .split_whitespace()
            .map(|word| word.strip_prefix("./").unwrap_or(word))
            .filter(|word| !word.is_empty())
            .map(|word| {
                let has_slash = word.contains('/');
                let name_part = word
                    .rsplit_once('/')
                    .map(|(_, tail)| tail)
                    .filter(|tail| !tail.is_empty())
                    .map(fuzzy_atom);
                Word {
                    text: word.to_string(),
                    atom: fuzzy_atom(word),
                    name_part,
                    has_slash,
                    case_sensitive: word.chars().any(char::is_uppercase),
                }
            })
            .collect();
        Query { words }
    }

    /// Whether the query has no words, in which case nothing is matched and the finder
    /// shows recent files instead.
    pub(crate) fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

fn fuzzy_atom(word: &str) -> Atom {
    Atom::new(
        word,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    )
}

/// Scores paths against a [`Query`]. It owns the matcher's scratch memory, so keep one
/// per thread and reuse it for every path.
pub(crate) struct Scorer {
    matcher: Matcher,
    chars: Vec<char>,
    indices: Vec<u32>,
}

impl Scorer {
    pub(crate) fn new() -> Scorer {
        Scorer {
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            chars: Vec::new(),
            indices: Vec::new(),
        }
    }

    /// The score of `path` (a relative path with `/` separators) for `query`, or `None`
    /// if a word does not match.
    pub(crate) fn score(&mut self, query: &Query, path: &str) -> Option<u32> {
        let name_start = name_start(path);
        let mut total = 0u32;
        for word in &query.words {
            total = total.saturating_add(self.score_word(word, path, name_start, None)?);
        }
        Some(total)
    }

    /// Like [`Scorer::score`], and also the `char` positions of the matched characters,
    /// ascending and without duplicates.
    pub(crate) fn score_with_indices(
        &mut self,
        query: &Query,
        path: &str,
    ) -> Option<(u32, Vec<usize>)> {
        let name_start = name_start(path);
        let mut total = 0u32;
        let mut positions = Vec::new();
        for word in &query.words {
            let score = self.score_word(word, path, name_start, Some(&mut positions))?;
            total = total.saturating_add(score);
        }
        positions.sort_unstable();
        positions.dedup();
        Some((total, positions))
    }

    fn score_word(
        &mut self,
        word: &Word,
        path: &str,
        name_start: usize,
        mut positions: Option<&mut Vec<usize>>,
    ) -> Option<u32> {
        let name = &path[name_start..];
        if word.has_slash {
            let score = self.match_text(&word.atom, path, 0, positions.as_deref_mut())?;
            let in_name = match &word.name_part {
                Some(name_part) => self.match_text(name_part, name, 0, None).is_some(),
                None => false,
            };
            return Some(score + if in_name { NAME_BONUS } else { 0 });
        }
        // The offset turns positions inside the file name into positions in the path.
        let name_offset = if positions.is_some() {
            path[..name_start].chars().count()
        } else {
            0
        };
        if let Some(score) =
            self.match_text(&word.atom, name, name_offset, positions.as_deref_mut())
        {
            return Some(NAME_BONUS + score + name_closeness(word, name));
        }
        self.match_text(&word.atom, path, 0, positions)
    }

    /// Match one atom against `text`. When `positions` is given, the matched `char`
    /// positions, shifted by `offset`, are appended to it.
    fn match_text(
        &mut self,
        atom: &Atom,
        text: &str,
        offset: usize,
        positions: Option<&mut Vec<usize>>,
    ) -> Option<u32> {
        // The haystack is built from `char`s (not grapheme clusters), so the positions
        // the matcher reports are `char` positions in `text`.
        let haystack = if text.is_ascii() {
            Utf32Str::Ascii(text.as_bytes())
        } else {
            self.chars.clear();
            self.chars.extend(text.chars());
            Utf32Str::Unicode(&self.chars)
        };
        match positions {
            None => atom.score(haystack, &mut self.matcher).map(u32::from),
            Some(positions) => {
                self.indices.clear();
                let score = atom.indices(haystack, &mut self.matcher, &mut self.indices)?;
                positions.extend(self.indices.iter().map(|index| *index as usize + offset));
                Some(u32::from(score))
            }
        }
    }
}

/// The byte offset at which the file name starts in a `/`-separated path.
fn name_start(path: &str) -> usize {
    path.rfind('/').map_or(0, |slash| slash + 1)
}

/// The extra score for how much of the file name a word that matched it covers.
fn name_closeness(word: &Word, name: &str) -> u32 {
    let same = |a: &str, b: &str| {
        if word.case_sensitive {
            a == b
        } else {
            equal_ignoring_case(a, b)
        }
    };
    if same(name, &word.text) {
        return EXACT_NAME_BONUS;
    }
    let stem = match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    };
    // A dotfile is usually asked for without its dot: `gitignore`, `env`.
    let undotted = name.strip_prefix('.').unwrap_or(name);
    if same(stem, &word.text) || same(undotted, &word.text) {
        return EXACT_STEM_BONUS;
    }
    let is_prefix = name
        .get(..word.text.len())
        .is_some_and(|start| same(start, &word.text));
    if is_prefix {
        NAME_PREFIX_BONUS
    } else {
        0
    }
}

fn equal_ignoring_case(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(b)
    } else {
        a.chars()
            .flat_map(char::to_lowercase)
            .eq(b.chars().flat_map(char::to_lowercase))
    }
}

/// A scored path, before the best ones are turned into [`FileMatch`]es.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Candidate<'a> {
    pub(crate) path: &'a str,
    pub(crate) score: u32,
    /// Position in the recent files list, `usize::MAX` for a file that is not in it.
    pub(crate) recent_rank: usize,
}

/// The order results are shown in: best score first, then recent files (most recent
/// first), then the shorter path, then alphabetically.
pub(crate) fn compare_candidates(a: &Candidate<'_>, b: &Candidate<'_>) -> Ordering {
    b.score
        .cmp(&a.score)
        .then_with(|| a.recent_rank.cmp(&b.recent_rank))
        .then_with(|| a.path.len().cmp(&b.path.len()))
        .then_with(|| a.path.cmp(b.path))
}

/// Keep the best `limit` candidates, in display order.
pub(crate) fn best_candidates<'a>(
    mut candidates: Vec<Candidate<'a>>,
    limit: usize,
) -> Vec<Candidate<'a>> {
    if limit == 0 {
        return Vec::new();
    }
    if candidates.len() > limit {
        candidates.select_nth_unstable_by(limit - 1, compare_candidates);
        candidates.truncate(limit);
    }
    candidates.sort_unstable_by(compare_candidates);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(path: &str, line: Option<u32>, col: Option<u32>) -> FileQuery {
        FileQuery {
            path_query: path.to_string(),
            line,
            col,
        }
    }

    #[test]
    fn parses_plain_paths() {
        assert_eq!(parse_query("main.rs"), query("main.rs", None, None));
        assert_eq!(
            parse_query("  src/main.rs  "),
            query("src/main.rs", None, None)
        );
        assert_eq!(parse_query(""), query("", None, None));
        assert_eq!(parse_query("   "), query("", None, None));
        assert_eq!(parse_query("two words"), query("two words", None, None));
    }

    #[test]
    fn parses_line_and_column() {
        assert_eq!(parse_query("main.rs:12"), query("main.rs", Some(12), None));
        assert_eq!(
            parse_query("main.rs:12:3"),
            query("main.rs", Some(12), Some(3))
        );
        assert_eq!(
            parse_query("src/a b.rs:7:1"),
            query("src/a b.rs", Some(7), Some(1))
        );
        assert_eq!(parse_query(":12"), query("", Some(12), None));
        assert_eq!(parse_query(":12:4"), query("", Some(12), Some(4)));
        assert_eq!(parse_query("main.rs:0"), query("main.rs", Some(0), None));
    }

    #[test]
    fn tolerates_colons_while_typing() {
        assert_eq!(parse_query("main.rs:"), query("main.rs", None, None));
        assert_eq!(parse_query("main.rs:12:"), query("main.rs", Some(12), None));
        assert_eq!(
            parse_query("main.rs:12:3:"),
            query("main.rs", Some(12), Some(3))
        );
        assert_eq!(parse_query("main.rs::12"), query("main.rs", Some(12), None));
        assert_eq!(parse_query(":"), query("", None, None));
    }

    #[test]
    fn only_numbers_are_positions() {
        assert_eq!(parse_query("main.rs:abc"), query("main.rs:abc", None, None));
        assert_eq!(
            parse_query("main.rs:12:abc"),
            query("main.rs:12:abc", None, None)
        );
        assert_eq!(parse_query("main.rs:1a"), query("main.rs:1a", None, None));
        assert_eq!(parse_query("main.rs:-3"), query("main.rs:-3", None, None));
        assert_eq!(parse_query("a:b:9"), query("a:b", Some(9), None));
        // Too large for a line number: left alone rather than wrapped.
        assert_eq!(
            parse_query("main.rs:99999999999"),
            query("main.rs:99999999999", None, None)
        );
        // Only the last two numbers are a position.
        assert_eq!(parse_query("a:1:2:3"), query("a:1", Some(2), Some(3)));
    }

    fn score(text: &str, path: &str) -> Option<u32> {
        Scorer::new().score(&Query::new(text), path)
    }

    fn indices(text: &str, path: &str) -> Vec<usize> {
        Scorer::new()
            .score_with_indices(&Query::new(text), path)
            .expect("the query matches")
            .1
    }

    fn highlighted(text: &str, path: &str) -> String {
        let positions = indices(text, path);
        path.chars()
            .enumerate()
            .filter(|(index, _)| positions.contains(index))
            .map(|(_, c)| c)
            .collect()
    }

    #[test]
    fn a_name_match_beats_a_directory_match() {
        let in_name = score("main", "tools/domain.rs").unwrap();
        let in_dir = score("main", "src/main/java/App.java").unwrap();
        assert!(in_name >= NAME_BONUS);
        assert!(in_dir < NAME_BONUS);
        assert!(in_name > in_dir);
        // Even a scattered name match outranks a perfect directory match.
        assert!(score("core", "x/c_o_r_e.txt").unwrap() > score("core", "core/lib.rs").unwrap());
    }

    #[test]
    fn exact_names_and_stems_come_first() {
        let exact = score("main.rs", "src/main.rs").unwrap();
        let stem = score("main", "src/main.rs").unwrap();
        let prefix = score("main", "src/main_loop.rs").unwrap();
        let middle = score("main", "src/old_main.rs").unwrap();
        let inside = score("main", "src/domain.rs").unwrap();
        assert!(stem > prefix, "{stem} > {prefix}");
        assert!(prefix > middle, "{prefix} > {middle}");
        assert!(middle > inside, "{middle} > {inside}");
        assert!(exact > score("main.rs", "src/main.rs.bak").unwrap());
        // Dotfiles are found by the name after the dot.
        assert!(
            score("gitignore", ".gitignore").unwrap()
                > score("gitignore", "docs/gitignore_notes.md").unwrap()
        );
    }

    #[test]
    fn consecutive_matches_beat_scattered_ones() {
        assert!(score("tree", "src/tree.rs").unwrap() > score("tree", "src/trie_eq.rs").unwrap());
    }

    #[test]
    fn every_word_has_to_match() {
        assert!(score("keys core", "crates/core/src/keys.rs").is_some());
        assert!(score("core keys", "crates/core/src/keys.rs").is_some());
        assert!(score("keys nothere", "crates/core/src/keys.rs").is_none());
        // Two words in the name beat one in the name and one in the directory.
        assert!(
            score("file tree", "src/file_tree.rs").unwrap()
                > score("file tree", "tree/file.rs").unwrap()
        );
    }

    #[test]
    fn a_word_can_span_directory_and_name() {
        let spanning = score("srcmain", "src/main.rs").unwrap();
        assert!(spanning < NAME_BONUS);
        assert!(score("srcmain", "docs/readme.md").is_none());
    }

    #[test]
    fn slashes_match_the_path_and_reward_the_name() {
        let named = score("core/keys", "crates/core/src/keys.rs").unwrap();
        let in_dirs = score("core/keys", "crates/core/keys/mod.rs").unwrap();
        assert!(named >= NAME_BONUS);
        assert!(in_dirs < NAME_BONUS);
        assert!(score("core/", "crates/core/src/keys.rs").unwrap() < NAME_BONUS);
        assert!(score("src/zzz", "src/main.rs").is_none());
    }

    #[test]
    fn matching_is_smart_case() {
        assert!(score("readme", "README.md").is_some());
        assert!(score("README", "README.md").is_some());
        assert!(score("README", "readme.md").is_none());
        assert!(score("Readme", "README.md").is_none());
        // Per word: only the capitalised word is strict.
        assert!(score("Cargo toml", "Cargo.TOML").is_some());
        assert!(score("cargo Toml", "Cargo.TOML").is_none());
    }

    #[test]
    fn indices_point_at_the_matched_characters() {
        assert_eq!(indices("main", "src/main.rs"), vec![4, 5, 6, 7]);
        assert_eq!(highlighted("mr", "src/main.rs"), "mr");
        assert_eq!(highlighted("srcmain", "src/main.rs"), "srcmain");
        assert_eq!(
            highlighted("core/keys", "crates/core/src/keys.rs"),
            "core/keys"
        );
        // Words are merged, sorted and deduplicated.
        assert_eq!(indices("rs main", "src/main.rs"), vec![4, 5, 6, 7, 9, 10]);
        assert_eq!(indices("ma ai", "main.rs"), vec![0, 1, 2]);
    }

    #[test]
    fn indices_are_char_positions_in_unicode_paths() {
        // "数据/日本語.txt": the name starts at char 3.
        assert_eq!(indices("txt", "数据/日本語.txt"), vec![7, 8, 9]);
        assert_eq!(indices("本", "数据/日本語.txt"), vec![4]);
        assert_eq!(indices("数", "数据/日本語.txt"), vec![0]);
        // A plain letter in the query matches its accented form in the path.
        assert_eq!(highlighted("cafe", "docs/café-menu.md"), "café");
        assert!(score("é", "docs/café.md").is_some());
        assert!(score("é", "docs/cafe.md").is_none());
    }

    #[test]
    fn empty_queries_have_no_words() {
        assert!(Query::new("").is_empty());
        assert!(Query::new("   ").is_empty());
        assert!(Query::new("./").is_empty());
        assert!(!Query::new("a").is_empty());
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn a_leading_dot_slash_is_ignored() {
        assert_eq!(
            score("./src/main.rs", "src/main.rs"),
            score("src/main.rs", "src/main.rs")
        );
    }

    fn candidate(path: &str, score: u32, recent_rank: usize) -> Candidate<'_> {
        Candidate {
            path,
            score,
            recent_rank,
        }
    }

    #[test]
    fn ties_break_by_recency_then_length_then_name() {
        let ordered = best_candidates(
            vec![
                candidate("b/mod.rs", 10, usize::MAX),
                candidate("a/mod.rs", 10, usize::MAX),
                candidate("long/path/mod.rs", 10, usize::MAX),
                candidate("zz/recent/mod.rs", 10, 1),
                candidate("zz/most_recent/mod.rs", 10, 0),
                candidate("better.rs", 11, usize::MAX),
            ],
            10,
        );
        let paths: Vec<&str> = ordered.iter().map(|c| c.path).collect();
        assert_eq!(
            paths,
            [
                "better.rs",
                "zz/most_recent/mod.rs",
                "zz/recent/mod.rs",
                "a/mod.rs",
                "b/mod.rs",
                "long/path/mod.rs",
            ]
        );
    }

    #[test]
    fn the_limit_keeps_the_best() {
        let candidates: Vec<Candidate<'_>> = ["e", "d", "c", "b", "a"]
            .iter()
            .enumerate()
            .map(|(index, path)| candidate(path, index as u32, usize::MAX))
            .collect();
        let best = best_candidates(candidates.clone(), 2);
        assert_eq!(best.iter().map(|c| c.path).collect::<Vec<_>>(), ["a", "b"]);
        assert!(best_candidates(candidates.clone(), 0).is_empty());
        assert_eq!(best_candidates(candidates, 99).len(), 5);
    }
}
