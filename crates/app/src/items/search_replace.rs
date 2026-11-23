//! Replacing one match of a project search in its file, for `search::ReplaceNext`.
//!
//! `bakedit_project::replace_all` rewrites every match in every file; replacing the one
//! match the user is looking at needs its own small routine. It reads the file, checks that
//! the match is still where the search found it, works out the replacement the way the
//! search's own matcher would (including `$1` references in regex mode) and writes the file
//! back in one atomic step. It blocks on the disk, so the view runs it on a worker.

use std::fs;
use std::path::Path;

use bakedit_project::fileops::write_atomic;
use bakedit_project::SearchOptions;
use regex::{Regex, RegexBuilder};

/// Where a match is: the 1-based line, and the char range within that line (counted from
/// the start of the real line, not of the clamped copy a search result carries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub line: u32,
    pub start: usize,
    pub end: usize,
    /// The text the search saw there; the replacement is refused if the file differs.
    pub expected: String,
}

const UTF8_BOM: char = '\u{feff}';

/// Replace the match at `target` in `path` and return the text that went in its place.
///
/// Errors (as sentences for the status line) when the file cannot be read as UTF-8, when it
/// changed since the search (the line is not as it was, or the file changed while this ran),
/// or when it cannot be written. Line endings and a byte order mark are kept.
pub fn replace_one(path: &Path, options: &SearchOptions, target: &Target, replacement: &str) -> Result<String, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let before = fs::metadata(path).map_err(|e| format!("Could not read {name}: {e}"))?;
    let bytes = fs::read(path).map_err(|e| format!("Could not read {name}: {e}"))?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{name} is not UTF-8 text, so it was left alone"))?;
    let (bom, body) = match text.strip_prefix(UTF8_BOM) {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text.as_str()),
    };
    let changed = || format!("{name} changed since the search: search again");

    let (line_start, line) = line_at(body, target.line).ok_or_else(changed)?;
    let content = line.strip_suffix('\n').unwrap_or(line);
    let content = content.strip_suffix('\r').unwrap_or(content);
    let start = char_to_byte(content, target.start).ok_or_else(changed)?;
    let end = char_to_byte(content, target.end).ok_or_else(changed)?;
    if content[start..end] != target.expected {
        return Err(changed());
    }

    let new_text = if options.regex { expand(content, start..end, options, replacement).ok_or_else(changed)? } else { replacement.to_string() };

    let mut rewritten = String::with_capacity(text.len() + new_text.len());
    rewritten.push_str(bom);
    rewritten.push_str(&body[..line_start + start]);
    rewritten.push_str(&new_text);
    rewritten.push_str(&body[line_start + end..]);

    // The file must not have been written by somebody else while this ran.
    match fs::metadata(path) {
        Ok(now) if now.len() == before.len() && now.modified().ok() == before.modified().ok() => {}
        _ => return Err(changed()),
    }
    write_atomic(path, rewritten.as_bytes()).map_err(|e| format!("Could not write {name}: {e}"))?;
    Ok(new_text)
}

/// The byte offset and text (with its line break) of 1-based line `number`.
fn line_at(body: &str, number: u32) -> Option<(usize, &str)> {
    let mut offset = 0;
    for (index, line) in body.split_inclusive('\n').enumerate() {
        if index as u32 + 1 == number {
            return Some((offset, line));
        }
        offset += line.len();
    }
    None
}

/// The byte offset of char `index` of `text`; `text.len()` for the end.
fn char_to_byte(text: &str, index: usize) -> Option<usize> {
    match text.char_indices().nth(index) {
        Some((byte, _)) => Some(byte),
        None if text.chars().count() == index => Some(text.len()),
        None => None,
    }
}

/// What the regex replacement `$1`, `${name}`, `$$` makes of the match at `span`, or `None`
/// when the pattern does not match exactly that span any more.
fn expand(line: &str, span: std::ops::Range<usize>, options: &SearchOptions, replacement: &str) -> Option<String> {
    // The search's matcher puts word boundaries around the pattern in whole-word mode; try
    // that first and the bare pattern second, so an odd pattern still has a way through.
    let mut attempts = Vec::new();
    if options.whole_word {
        attempts.push(format!(r"\b(?:{})\b", options.pattern));
    }
    attempts.push(options.pattern.clone());
    for pattern in attempts {
        let Some(regex) = build(&pattern, options.case_sensitive) else { continue };
        let Some(captures) = regex.captures_at(line, span.start) else { continue };
        let whole = captures.get(0)?;
        if whole.start() == span.start && whole.end() == span.end {
            let mut out = String::new();
            captures.expand(replacement, &mut out);
            return Some(out);
        }
    }
    None
}

fn build(pattern: &str, case_sensitive: bool) -> Option<Regex> {
    RegexBuilder::new(pattern).case_insensitive(!case_sensitive).multi_line(true).crlf(true).build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        fs::write(&path, contents).unwrap();
        (dir, path)
    }

    fn target(line: u32, start: usize, end: usize, expected: &str) -> Target {
        Target { line, start, end, expected: expected.to_string() }
    }

    #[test]
    fn replaces_only_the_one_match() {
        let (_dir, path) = file("foo foo\nfoo\n");
        let options = SearchOptions::new("foo");
        assert_eq!(replace_one(&path, &options, &target(1, 4, 7, "foo"), "bar").unwrap(), "bar");
        assert_eq!(fs::read_to_string(&path).unwrap(), "foo bar\nfoo\n");
    }

    #[test]
    fn keeps_crlf_and_the_byte_order_mark() {
        let (_dir, path) = file("\u{feff}a foo\r\nb\r\n");
        let options = SearchOptions::new("foo");
        replace_one(&path, &options, &target(1, 2, 5, "foo"), "é").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "\u{feff}a é\r\nb\r\n");
    }

    #[test]
    fn regex_replacements_expand_groups() {
        let (_dir, path) = file("let x = 10;\n");
        let options = SearchOptions { regex: true, ..SearchOptions::new(r"(\w+) = (\d+)") };
        let new = replace_one(&path, &options, &target(1, 4, 10, "x = 10"), "$2 = $1").unwrap();
        assert_eq!(new, "10 = x");
        assert_eq!(fs::read_to_string(&path).unwrap(), "let 10 = x;\n");
    }

    #[test]
    fn literal_replacements_are_not_expanded() {
        let (_dir, path) = file("cost\n");
        let options = SearchOptions::new("cost");
        replace_one(&path, &options, &target(1, 0, 4, "cost"), "$1 ${a} $$").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "$1 ${a} $$\n");
    }

    #[test]
    fn whole_word_regexes_expand_too() {
        let (_dir, path) = file("a cat concat\n");
        let options = SearchOptions { regex: true, whole_word: true, ..SearchOptions::new("c(a)t") };
        replace_one(&path, &options, &target(1, 2, 5, "cat"), "d${1}d").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a dad concat\n");
    }

    #[test]
    fn a_changed_file_is_refused_and_left_alone() {
        let (_dir, path) = file("foo\nzzz\n");
        let options = SearchOptions::new("foo");
        let error = replace_one(&path, &options, &target(1, 0, 3, "foo"), "x");
        assert!(error.is_ok());
        let again = replace_one(&path, &options, &target(1, 0, 3, "foo"), "y").unwrap_err();
        assert!(again.contains("changed since the search"), "{again}");
        assert!(replace_one(&path, &options, &target(9, 0, 3, "foo"), "y").unwrap_err().contains("changed"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "x\nzzz\n");
    }

    #[test]
    fn binary_and_missing_files_give_sentences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.bin");
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        let options = SearchOptions::new("a");
        assert!(replace_one(&path, &options, &target(1, 0, 1, "a"), "b").unwrap_err().contains("not UTF-8"));
        assert!(replace_one(&dir.path().join("gone"), &options, &target(1, 0, 1, "a"), "b").unwrap_err().contains("Could not read"));
    }

    #[test]
    fn empty_matches_insert() {
        let (_dir, path) = file("abc\n");
        let options = SearchOptions { regex: true, ..SearchOptions::new("^") };
        replace_one(&path, &options, &target(1, 0, 0, ""), "> ").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "> abc\n");
    }
}
