//! Finding file references and URLs in a line of terminal text, and resolving the file
//! references against the filesystem.
//!
//! Detection is pure and purely syntactic: [`detect_links`] never touches the disk, so a
//! [`LinkTarget::Path`] is a *candidate*. `and/or` looks exactly like `src/ui`, and only
//! [`LinkTarget::resolve`] (or [`resolve_path`]) can tell them apart. Resolve a candidate
//! before presenting it as a link; do that on hover or click, not for every frame, because
//! it costs a few `stat` calls per search base.
//!
//! Columns are `char` offsets into the line, half-open: a span covers `start..end`.

use std::path::{Component, Path, PathBuf};

/// What a link points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A file or directory reference.
    Path {
        /// The path as written: relative, absolute or starting with `~`. Not checked
        /// against the disk.
        path: String,
        /// The 1-based line the reference names, if any.
        line: Option<u32>,
        /// The 1-based column the reference names, if any.
        col: Option<u32>,
    },
    /// A URL. Found in running text it is always `http` or `https`. As the target of a
    /// Markdown link or of a hyperlink a program made (OSC 8) it can have any scheme at
    /// all, and it comes from whatever wrote to the terminal: check the scheme before
    /// handing it to the system to open.
    Url(String),
}

/// A link found in a line of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    /// First `char` column of the link.
    pub start: usize,
    /// One past the last `char` column of the link.
    pub end: usize,
    /// What the link points at.
    pub target: LinkTarget,
}

/// A file reference that exists on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// The absolute path, with `.` and `..` components removed.
    pub path: PathBuf,
    /// The 1-based line the reference names, if any.
    pub line: Option<u32>,
    /// The 1-based column the reference names, if any.
    pub col: Option<u32>,
}

/// Find every link in one line of text, in order of appearance.
///
/// Recognised file references:
///
/// - paths with a directory part: `src/main.rs`, `/abs/file`, `~/notes`, `./a`, `../b`,
///   including bracketed route segments such as `app/(auth)/[id]/page.tsx`
/// - bare file names with a well-known extension (`Cargo.toml`), dot files (`.gitignore`)
///   and a few extension-less names (`Makefile`)
/// - a position after any of those: `:42`, `:42:7`, `:42-68`, `#L42`, `#L42-L68`,
///   `(42,7)`, and `", line 42` as in a Python traceback
/// - `file://` URLs, percent-decoded
/// - Markdown links, `[label](target)`: the span is the whole construct and the target is
///   what the parentheses hold
/// - a quoted path with spaces, when it starts with `/`, `~/`, `./` or `../`
///
/// and `http`/`https` URLs, with balanced parentheses kept and trailing punctuation left
/// out. Wrapping quotes, backticks, brackets and trailing punctuation are never part of a
/// span, and anything that is not a path character (box-drawing characters and `|`
/// included) ends a token.
pub fn detect_links(line: &str) -> Vec<LinkSpan> {
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        i = scan_at(&chars, i, &mut spans).max(i + 1);
    }
    spans
}

/// The link covering `char_col` in `line`, if there is one.
pub fn link_at(line: &str, char_col: usize) -> Option<LinkSpan> {
    detect_links(line)
        .into_iter()
        .find(|span| span.start <= char_col && char_col < span.end)
}

// ---------------------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------------------

/// Look for a link starting at `i`. Returns where scanning continues.
fn scan_at(chars: &[char], i: usize, spans: &mut Vec<LinkSpan>) -> usize {
    let c = chars[i];
    if c == '['
        && !chars[..i]
            .last()
            .is_some_and(|p| p.is_alphanumeric() || matches!(p, '_' | ')' | ']'))
    {
        if let Some((end, target)) = markdown_link(chars, i) {
            spans.push(LinkSpan {
                start: i,
                end,
                target,
            });
            return end;
        }
    }
    if let Some(scheme) = scheme_len(chars, i) {
        let end = url_end(chars, i);
        if end > i + scheme {
            let text: String = chars[i..end].iter().collect();
            if let Some(target) = url_target(&text) {
                spans.push(LinkSpan {
                    start: i,
                    end,
                    target,
                });
            }
            return end;
        }
    }
    if matches!(c, '"' | '\'' | '`') {
        if let Some((close, target)) = quoted_path(chars, i) {
            spans.push(LinkSpan {
                start: i + 1,
                end: close,
                target,
            });
            return close + 1;
        }
        return i + 1;
    }
    if !is_token_char(c) || matches!(c, '(' | ')' | '[' | ']' | ':' | '=') {
        return i + 1;
    }
    let end = token_end(chars, i);
    if let Some(span) = path_in_token(chars, i, end) {
        spans.push(span);
    }
    end
}

fn is_combining_mark(c: char) -> bool {
    ('\u{300}'..='\u{36f}').contains(&c)
}

/// Characters a path token is made of. Everything else ends a token.
fn is_token_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            '/' | '.'
                | '_'
                | '-'
                | '~'
                | '+'
                | '@'
                | '%'
                | '#'
                | '$'
                | '='
                | ':'
                | '('
                | ')'
                | '['
                | ']'
        )
        || is_combining_mark(c)
}

/// Where the token starting at `start` ends.
///
/// Brackets and parentheses belong to the token only as a whole path segment (`/(group)/`,
/// `/[id]/`), and a colon only when a digit follows it (`:42`).
fn token_end(chars: &[char], start: usize) -> usize {
    let mut open: Vec<(usize, char)> = Vec::new();
    let mut j = start;
    while j < chars.len() {
        let c = chars[j];
        match c {
            '(' | '[' => {
                let previous = if j > start { chars[j - 1] } else { ' ' };
                if previous != '/' && previous != c {
                    break;
                }
                open.push((j, c));
            }
            ')' | ']' => {
                let opener = if c == ')' { '(' } else { '[' };
                match open.last() {
                    Some(&(_, last)) if last == opener => {
                        open.pop();
                    }
                    _ => break,
                }
            }
            ':' => {
                if !chars.get(j + 1).is_some_and(char::is_ascii_digit) {
                    break;
                }
            }
            _ if !is_token_char(c) => break,
            _ => {
                // A URL glued to the end of something else starts its own token.
                if j > start && !is_scheme_char(chars[j - 1]) && scheme_len(chars, j).is_some() {
                    break;
                }
            }
        }
        j += 1;
    }
    // A bracket that never closed was not part of a path.
    match open.first() {
        Some(&(position, _)) => position,
        None => j,
    }
}

/// The file reference inside the token `chars[start..end]`, if it holds one.
fn path_in_token(chars: &[char], start: usize, end: usize) -> Option<LinkSpan> {
    let mut start = start;
    let mut end = end;
    // A full stop after the reference ends the sentence, not the file name, and the `$`,
    // `%` or `#` after a directory is the end of a shell prompt.
    while end > start {
        let relative_dots = chars[end - 1] == '.' && end - start >= 2 && chars[end - 2] == '/';
        if !matches!(chars[end - 1], '.' | '$' | '%' | '#') || relative_dots {
            break;
        }
        end -= 1;
    }
    // `--flag=path` and `KEY=path`: the path is the value.
    while let Some(offset) = chars[start..end].iter().position(|c| *c == '=') {
        if chars[start..start + offset].contains(&'/') {
            break;
        }
        start += offset + 1;
    }
    if start >= end {
        return None;
    }
    let text: String = chars[start..end].iter().collect();
    let (path, mut line, mut col) = split_position(&text);
    if line.is_none() {
        if let Some((after, l, c)) = paren_position(chars, end) {
            end = after;
            line = Some(l);
            col = c;
        } else if let Some((l, c)) = worded_position(chars, end) {
            line = Some(l);
            col = c;
        }
    }
    let called = chars.get(end) == Some(&'(');
    let closing_tag = start > 0 && chars[start - 1] == '<' && is_tag_name(path);
    if closing_tag || !is_path_like(path, line.is_some(), called) {
        return None;
    }
    Some(LinkSpan {
        start,
        end,
        target: LinkTarget::Path {
            path: path.to_string(),
            line,
            col,
        },
    })
}

/// `/div` after a `<`: the end of an HTML tag, not a path.
fn is_tag_name(path: &str) -> bool {
    path.strip_prefix('/').is_some_and(|name| {
        name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// `(12)` or `(12,5)` straight after a path, as TypeScript and MSVC print positions.
/// Returns the index after the closing parenthesis.
fn paren_position(chars: &[char], at: usize) -> Option<(usize, u32, Option<u32>)> {
    if chars.get(at) != Some(&'(') {
        return None;
    }
    let (line, mut j) = number_at(chars, at + 1)?;
    let mut col = None;
    if chars.get(j) == Some(&',') {
        let (c, after) = number_at(chars, j + 1)?;
        col = Some(c);
        j = after;
    }
    (chars.get(j) == Some(&')')).then_some((j + 1, line, col))
}

/// `", line 12` (Python), ` line 12`, ` on line 12` (PHP) after a path, optionally followed
/// by `, column 5`.
fn worded_position(chars: &[char], at: usize) -> Option<(u32, Option<u32>)> {
    let mut j = at;
    if matches!(chars.get(j), Some('"' | '\'' | '`')) {
        j += 1;
    }
    if chars.get(j) == Some(&',') {
        j += 1;
    }
    j = skip_word(chars, j, " on").unwrap_or(j);
    j = skip_word(chars, j, " line ")?;
    let (line, after) = number_at(chars, j)?;
    let mut k = after;
    if chars.get(k) == Some(&',') {
        k += 1;
    }
    let col = [" column ", " col ", " char "].iter().find_map(|word| {
        let k = skip_word(chars, k, word)?;
        number_at(chars, k).map(|(c, _)| c)
    });
    Some((line, col))
}

fn skip_word(chars: &[char], at: usize, word: &str) -> Option<usize> {
    let mut j = at;
    for expected in word.chars() {
        if chars.get(j) != Some(&expected) {
            return None;
        }
        j += 1;
    }
    Some(j)
}

/// A decimal number starting at `at`: its value and the index after it.
fn number_at(chars: &[char], at: usize) -> Option<(u32, usize)> {
    let mut j = at;
    let mut value: u32 = 0;
    while let Some(digit) = chars.get(j).and_then(|c| c.to_digit(10)) {
        value = value.checked_mul(10)?.checked_add(digit)?;
        j += 1;
    }
    (j > at).then_some((value, j))
}

/// A quoted string holding a path with spaces in it: `"/Users/me/My Notes/todo.md"`.
/// Returns the index of the closing quote.
fn quoted_path(chars: &[char], open: usize) -> Option<(usize, LinkTarget)> {
    const MAX_LEN: usize = 400;
    let quote = chars[open];
    let window = &chars[open + 1..chars.len().min(open + 1 + MAX_LEN)];
    let len = window.iter().position(|c| *c == quote)?;
    let close = open + 1 + len;
    let content: String = window[..len].iter().collect();
    let rooted = ["/", "~/", "./", "../"]
        .iter()
        .any(|prefix| content.starts_with(prefix));
    if !rooted || !content.contains(' ') || content.ends_with(' ') {
        return None;
    }
    // A command line is not a path: `"/usr/bin/env -u X"`, `'/a /b'`.
    let command_like = content.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                '"' | '\'' | '`' | '|' | '&' | ';' | '<' | '>' | '$' | '*'
            )
    }) || [" -", " /", " ~", " ."]
        .iter()
        .any(|next| content.contains(next));
    if command_like {
        return None;
    }
    let (path, mut line, mut col) = split_position(&content);
    if line.is_none() {
        if let Some((l, c)) = worded_position(chars, close) {
            line = Some(l);
            col = c;
        }
    }
    Some((
        close,
        LinkTarget::Path {
            path: path.to_string(),
            line,
            col,
        },
    ))
}

// ---------------------------------------------------------------------------------------
// URLs and Markdown
// ---------------------------------------------------------------------------------------

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// The length of `scheme://` when one starts at `i`.
fn scheme_len(chars: &[char], i: usize) -> Option<usize> {
    if !chars.get(i)?.is_ascii_alphabetic() {
        return None;
    }
    let mut j = i + 1;
    while chars.get(j).is_some_and(|c| is_scheme_char(*c)) {
        j += 1;
    }
    let slashes = chars.get(j) == Some(&':')
        && chars.get(j + 1) == Some(&'/')
        && chars.get(j + 2) == Some(&'/');
    (j - i >= 2 && slashes).then_some(j + 3 - i)
}

fn is_url_char(c: char) -> bool {
    c.is_alphanumeric()
        || is_combining_mark(c)
        || matches!(
            c,
            '-' | '.'
                | '_'
                | '~'
                | ':'
                | '/'
                | '?'
                | '#'
                | '['
                | ']'
                | '@'
                | '!'
                | '$'
                | '&'
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | ';'
                | '='
                | '%'
        )
}

/// Where the URL starting at `start` ends: the longest run of URL characters, minus the
/// punctuation and unbalanced closing brackets at its end.
fn url_end(chars: &[char], start: usize) -> usize {
    let mut end = start;
    while chars.get(end).is_some_and(|c| is_url_char(*c)) {
        end += 1;
    }
    trim_url_end(chars, start, end)
}

fn trim_url_end(chars: &[char], start: usize, mut end: usize) -> usize {
    let count = |range: &[char], wanted: char| range.iter().filter(|c| **c == wanted).count();
    while end > start {
        let url = &chars[start..end];
        let drop = match chars[end - 1] {
            '.' | ',' | ';' | ':' | '!' | '?' | '*' => true,
            ')' => count(url, '(') < count(url, ')'),
            ']' => count(url, '[') < count(url, ']'),
            _ => false,
        };
        if !drop {
            break;
        }
        end -= 1;
    }
    end
}

/// The target of a `scheme://...` URL found in running text: web URLs and `file://` paths.
/// Other schemes are recognised, so they are not mistaken for paths, but are not links.
fn url_target(text: &str) -> Option<LinkTarget> {
    let (scheme, rest) = text.split_once("://")?;
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => {
            let host_starts = rest
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '[');
            host_starts.then(|| LinkTarget::Url(text.to_string()))
        }
        "file" => file_url_target(rest),
        _ => None,
    }
}

/// `host/path#L10` from a `file://` URL: the percent-decoded path and its position.
fn file_url_target(rest: &str) -> Option<LinkTarget> {
    let path_start = rest.find('/')?;
    let location = &rest[path_start..];
    let (encoded, mut line, mut col) = match location.split_once('#') {
        Some((path, fragment)) => {
            let (line, col) = fragment_position(fragment).unwrap_or((None, None));
            (path, line, col)
        }
        None => (location, None, None),
    };
    let mut path = percent_decode(encoded);
    if line.is_none() {
        let (plain, l, c) = split_position(&path);
        if l.is_some() {
            line = l;
            col = c;
            path = plain.to_string();
        }
    }
    (path.len() > 1).then_some(LinkTarget::Path { path, line, col })
}

/// The target of an OSC 8 hyperlink: a `file://` URI is a path, anything else is handed on
/// as a URL.
pub(crate) fn hyperlink_target(uri: &str) -> Option<LinkTarget> {
    match uri.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("file") => file_url_target(rest),
        _ if uri.is_empty() => None,
        _ => Some(LinkTarget::Url(uri.to_string())),
    }
}

/// A Markdown link `[label](target)` starting at the `[` at `open`. Returns the index after
/// the closing parenthesis and what the target points at.
fn markdown_link(chars: &[char], open: usize) -> Option<(usize, LinkTarget)> {
    const MAX_LABEL: usize = 300;
    let mut depth = 0usize;
    let mut close = None;
    for (offset, c) in chars[open..].iter().enumerate().take(MAX_LABEL) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let target_start = close + 2;
    let (target_end, after) = if chars.get(target_start) == Some(&'<') {
        let len = chars[target_start..].iter().position(|c| *c == '>')?;
        let end = target_start + len;
        (end, markdown_close(chars, end + 1)?)
    } else {
        let mut depth = 0usize;
        let mut j = target_start;
        loop {
            match *chars.get(j)? {
                c if c.is_whitespace() => break (j, markdown_close(chars, j)?),
                '(' => depth += 1,
                ')' if depth == 0 => break (j, j + 1),
                ')' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
    };
    let first = if chars.get(target_start) == Some(&'<') {
        target_start + 1
    } else {
        target_start
    };
    let target: String = chars[first..target_end].iter().collect();
    markdown_target(&target).map(|target| (after, target))
}

/// Skip an optional quoted title and the closing parenthesis of a Markdown link. Returns
/// the index after the parenthesis.
fn markdown_close(chars: &[char], at: usize) -> Option<usize> {
    let mut j = at;
    while chars.get(j).is_some_and(|c| c.is_whitespace()) {
        j += 1;
    }
    if let Some(quote @ ('"' | '\'')) = chars.get(j).copied() {
        let len = chars[j + 1..].iter().position(|c| *c == quote)?;
        j += len + 2;
        while chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
    }
    (chars.get(j) == Some(&')')).then_some(j + 1)
}

fn markdown_target(target: &str) -> Option<LinkTarget> {
    if target.is_empty() || target.starts_with('#') {
        return None;
    }
    if let Some((scheme, rest)) = target.split_once(':') {
        let is_scheme = scheme.len() >= 2
            && scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(is_scheme_char)
            && !rest.chars().next().is_some_and(|c| c.is_ascii_digit());
        if is_scheme {
            return match rest.strip_prefix("//") {
                Some(location) if scheme.eq_ignore_ascii_case("file") => file_url_target(location),
                // `[x](main.rs:fn)` is nothing; `mailto:` and `zed://` are for the system.
                None if !rest.contains('@') && !rest.starts_with('/') => None,
                _ => Some(LinkTarget::Url(target.to_string())),
            };
        }
    }
    let decoded = percent_decode(target);
    let (path, line, col) = split_position(&decoded);
    path.chars()
        .any(char::is_alphanumeric)
        .then(|| LinkTarget::Path {
            path: path.to_string(),
            line,
            col,
        })
}

/// Decode `%XX` escapes. Invalid escapes are kept as written and invalid UTF-8 is replaced.
pub(crate) fn percent_decode(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (
            bytes[i],
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------------------
// Positions
// ---------------------------------------------------------------------------------------

/// Split a reference as written into the path and the position after it:
/// `a.rs:42`, `a.rs:42:7`, `a.rs:42-68`, `a.rs:42:7-42:9`, `a.rs#L42`, `a.rs#L42-L68`,
/// `a.rs#L42C7`. Text with no position comes back whole.
fn split_position(text: &str) -> (&str, Option<u32>, Option<u32>) {
    if let Some(index) = text.rfind("#L") {
        if index > 0 {
            if let Some((line, col)) = fragment_position(&text[index + 1..]) {
                return (&text[..index], line, col);
            }
        }
    }
    colon_position(text).unwrap_or((text, None, None))
}

/// The position in a URL fragment: `L42`, `L42-L68`, `L42C7`, `L42C7-L43C1`.
fn fragment_position(fragment: &str) -> Option<(Option<u32>, Option<u32>)> {
    let rest = fragment.strip_prefix('L')?;
    let (line, rest) = leading_number(rest)?;
    let (col, rest) = match rest.strip_prefix('C').and_then(leading_number) {
        Some((col, rest)) => (Some(col), rest),
        None => (None, rest),
    };
    if !rest.is_empty() {
        // The end of a range, which only has to be well formed.
        let end = rest.strip_prefix('-')?;
        let end = end.strip_prefix('L').unwrap_or(end);
        let (_, end) = leading_number(end)?;
        let end = match end.strip_prefix('C').and_then(leading_number) {
            Some((_, end)) => end,
            None => end,
        };
        if !end.is_empty() {
            return None;
        }
    }
    Some((Some(line), col))
}

/// `path:line`, `path:line:col`, either followed by the end of a range.
fn colon_position(text: &str) -> Option<(&str, Option<u32>, Option<u32>)> {
    let mut body = text;
    // `-68` or `-42:9` after a position is the end of a range.
    if let Some((before, end)) = text.rsplit_once('-') {
        let end_is_position = match end.split_once(':') {
            Some((line, col)) => is_number(line) && is_number(col),
            None => is_number(end),
        };
        if end_is_position && trailing_number(before).is_some() {
            body = before;
        }
    }
    let (rest, last) = trailing_number(body)?;
    match trailing_number(rest) {
        Some((path, line)) if !path.is_empty() => Some((path, Some(line), Some(last))),
        _ if !rest.is_empty() => Some((rest, Some(last), None)),
        _ => None,
    }
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) && text.parse::<u32>().is_ok()
}

/// The number at the start of `text` and what follows it.
fn leading_number(text: &str) -> Option<(u32, &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let value = text[..digits].parse().ok()?;
    Some((value, &text[digits..]))
}

/// `:number` at the end of `text`: what precedes it, and the number.
fn trailing_number(text: &str) -> Option<(&str, u32)> {
    let (rest, digits) = text.rsplit_once(':')?;
    is_number(digits).then(|| (rest, digits.parse().unwrap_or(0)))
}

// ---------------------------------------------------------------------------------------
// What counts as a path
// ---------------------------------------------------------------------------------------

/// Whether `path` is worth offering as a file reference.
fn is_path_like(path: &str, has_position: bool, called: bool) -> bool {
    if !path.chars().any(char::is_alphanumeric) {
        return false;
    }
    if !path.contains('/') {
        // `console.log(...)` is a call, not a file.
        return !called && is_file_name(path, has_position);
    }
    if path.starts_with("//") {
        return false;
    }
    // Dates and fractions: 2026/11/22, 1/2, 24/7.
    let numeric = |part: &str| {
        part.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | ':'))
    };
    !path.split('/').all(numeric)
}

/// Whether a name with no directory part reads as a file name.
fn is_file_name(name: &str, has_position: bool) -> bool {
    let plain = |c: char| {
        c.is_alphanumeric()
            || is_combining_mark(c)
            || matches!(c, '.' | '_' | '-' | '+' | '@' | '~')
    };
    if !name.chars().all(plain) {
        return false;
    }
    if is_well_known_name(name) {
        return true;
    }
    // Abbreviations: e.g, i.e, U.S.A
    if name.split('.').all(|part| part.chars().count() <= 1) {
        return false;
    }
    if let Some(rest) = name.strip_prefix('.') {
        // A dot file, but not a bare extension such as `.rs`.
        let lower_start = rest.chars().next().is_some_and(|c| c.is_ascii_lowercase());
        return lower_start
            && rest.len() >= 2
            && !rest.ends_with('.')
            && (rest == "env" || !is_known_extension(rest));
    }
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || extension.is_empty() || !stem.chars().any(char::is_alphanumeric) {
        return false;
    }
    let extension = extension.to_ascii_lowercase();
    if is_known_extension(&extension) {
        return true;
    }
    // With a line number behind it, any extension will do, unless it reads as a host name
    // and a port.
    has_position
        && extension.len() <= 10
        && extension.chars().all(|c| c.is_ascii_alphanumeric())
        && extension
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && !is_top_level_domain(&extension)
}

fn is_well_known_name(name: &str) -> bool {
    matches!(
        name,
        "Makefile"
            | "makefile"
            | "GNUmakefile"
            | "Dockerfile"
            | "Containerfile"
            | "Justfile"
            | "justfile"
            | "Rakefile"
            | "Gemfile"
            | "Procfile"
            | "Vagrantfile"
            | "Brewfile"
            | "Jenkinsfile"
            | "Tiltfile"
            | "Caddyfile"
            | "Earthfile"
            | "LICENSE"
            | "LICENCE"
            | "COPYING"
            | "README"
            | "CHANGELOG"
            | "CONTRIBUTING"
            | "AUTHORS"
            | "NOTICE"
            | "CODEOWNERS"
    )
}

/// File extensions common enough that `name.ext` on its own is taken for a file. Lower case.
fn is_known_extension(extension: &str) -> bool {
    matches!(
        extension,
        // Rust, config and docs
        "rs" | "toml" | "lock" | "md" | "mdx" | "markdown" | "txt" | "rst" | "adoc" | "org" | "tex" | "bib"
        | "json" | "jsonc" | "json5" | "jsonl" | "ndjson" | "yaml" | "yml" | "xml" | "ini" | "cfg" | "conf"
        | "config" | "properties" | "plist" | "env" | "editorconfig" | "kdl" | "ron" | "cue" | "nix" | "hcl"
        | "tf" | "tfvars"
        // Web
        | "html" | "htm" | "xhtml" | "css" | "scss" | "sass" | "less" | "js" | "jsx" | "mjs" | "cjs" | "ts"
        | "tsx" | "mts" | "cts" | "vue" | "svelte" | "astro" | "wasm" | "map" | "ejs" | "hbs" | "pug" | "njk"
        | "liquid" | "twig" | "tmpl" | "tpl" | "j2" | "jinja" | "graphql" | "gql" | "prisma"
        // Other languages
        | "py" | "pyi" | "pyx" | "ipynb" | "rb" | "erb" | "rake" | "gemspec" | "go" | "mod" | "sum" | "c"
        | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "m" | "mm" | "cs" | "fs" | "fsx" | "vb"
        | "java" | "kt" | "kts" | "scala" | "sbt" | "groovy" | "gradle" | "clj" | "cljs" | "cljc" | "edn"
        | "ex" | "exs" | "erl" | "hrl" | "hs" | "lhs" | "ml" | "mli" | "elm" | "swift" | "dart" | "lua"
        | "pl" | "pm" | "php" | "r" | "jl" | "nim" | "zig" | "v" | "sv" | "vhd" | "asm" | "s" | "d"
        | "sol" | "gleam" | "odin" | "cr" | "el" | "lisp" | "scm" | "rkt" | "vim" | "awk" | "sed"
        // Shell, build and data
        | "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" | "mk" | "cmake" | "ninja" | "bazel"
        | "bzl" | "dockerfile" | "sql" | "proto" | "thrift" | "csv" | "tsv" | "parquet" | "log" | "out"
        | "diff" | "patch" | "snap" | "service"
        // Media and archives
        | "svg" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "pdf" | "zip" | "tar" | "gz"
        | "tgz" | "bz2" | "xz" | "zst" | "7z" | "jar" | "so" | "dylib" | "dll" | "a" | "o" | "exe" | "dmg"
        | "whl"
    )
}

fn is_top_level_domain(extension: &str) -> bool {
    matches!(
        extension,
        "com"
            | "org"
            | "net"
            | "io"
            | "dev"
            | "app"
            | "co"
            | "uk"
            | "us"
            | "de"
            | "fr"
            | "jp"
            | "cn"
            | "ru"
            | "eu"
            | "ca"
            | "au"
            | "nl"
            | "se"
            | "no"
            | "fi"
            | "dk"
            | "ch"
            | "at"
            | "be"
            | "es"
            | "it"
            | "pt"
            | "pl"
            | "br"
            | "in"
            | "kr"
            | "me"
            | "tv"
            | "ai"
            | "xyz"
            | "edu"
            | "gov"
            | "mil"
            | "int"
            | "info"
            | "biz"
            | "local"
            | "localhost"
            | "localdomain"
            | "internal"
            | "lan"
            | "home"
            | "test"
            | "example"
            | "invalid"
            | "cloud"
            | "tech"
            | "site"
            | "online"
    )
}

// ---------------------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------------------

impl LinkTarget {
    /// Resolve a [`LinkTarget::Path`] to a file or directory that exists. `None` for URLs
    /// and for paths that are not on disk.
    ///
    /// A file whose own name ends in digits and colons is kept whole: when `notes:12` is a
    /// file in the first base (or absolute), `notes:12` opens that file rather than line 12
    /// of `notes`. Otherwise this is [`resolve_path`] on the path, with the position
    /// carried over.
    pub fn resolve(&self, bases: &[PathBuf]) -> Option<ResolvedPath> {
        let LinkTarget::Path { path, line, col } = self else {
            return None;
        };
        let home = home_dir();
        if let Some(line) = line {
            let mut literals = vec![format!("{path}:{line}")];
            if let Some(col) = col {
                literals.insert(0, format!("{path}:{line}:{col}"));
            }
            for literal in literals {
                if let Some(found) = literal_path(&literal, bases, home.as_deref()) {
                    return Some(ResolvedPath {
                        path: found,
                        line: None,
                        col: None,
                    });
                }
            }
        }
        let found = resolve_with_home(path, bases, home.as_deref())?;
        Some(ResolvedPath {
            path: found,
            line: *line,
            col: *col,
        })
    }
}

/// Resolve a reference as someone would type it on a command line: `src/a.rs:10`,
/// `a.rs:10:4`, `a.rs:10-25`, `a.rs#L10-L25`, `file:///abs/a.rs#L10`.
///
/// This is `parse_target` from the `zed-show` script: a `file://` prefix is dropped (and the
/// rest percent-decoded), a reference that names an existing file as it stands is kept whole
/// even if it ends in digits and colons, and otherwise the position is split off and the
/// path looked up with [`resolve_path`]. `None` when nothing by that name exists.
pub fn resolve_reference(raw: &str, bases: &[PathBuf]) -> Option<ResolvedPath> {
    let home = home_dir();
    let plain = match raw.strip_prefix("file://") {
        Some(location) => {
            let path_start = location.find('/').unwrap_or(0);
            percent_decode(&location[path_start..])
        }
        None => raw.to_string(),
    };
    if let Some(found) = literal_path(&plain, bases, home.as_deref()) {
        return Some(ResolvedPath {
            path: found,
            line: None,
            col: None,
        });
    }
    let (path, line, col) = split_position(&plain);
    let found = resolve_with_home(path, bases, home.as_deref())?;
    Some(ResolvedPath {
        path: found,
        line,
        col,
    })
}

/// Find the file or directory a path as written refers to. Only existing paths are
/// returned, absolute and with `.` and `..` components removed (symlinks are not followed).
///
/// `~` and `~/...` expand to `$HOME`. An absolute path is taken as it is. A relative path
/// is tried against each of `bases` in order; when none has it, the same is tried without a
/// leading `a/` or `b/` (the prefixes `git diff` prints) and without a leading `@` (how
/// agents mention files).
pub fn resolve_path(raw: &str, bases: &[PathBuf]) -> Option<PathBuf> {
    resolve_with_home(raw, bases, home_dir().as_deref())
}

/// Where to look for the relative paths a terminal prints, most specific first: `cwd`, the
/// git repository above it, `~/Code`, every directory directly under `~/Code`, then `~`.
/// This is `search_bases` from the `zed-show` script. Duplicates are dropped.
pub fn default_search_bases(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut bases = vec![cwd.to_path_buf()];
    if let Some(root) = git_root(cwd) {
        bases.push(root);
    }
    let code = home.join("Code");
    bases.push(code.clone());
    if let Ok(entries) = std::fs::read_dir(&code) {
        let mut projects: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        projects.sort();
        bases.extend(projects);
    }
    bases.push(home.to_path_buf());
    let mut unique: Vec<PathBuf> = Vec::with_capacity(bases.len());
    for base in bases {
        if !base.as_os_str().is_empty() && !unique.contains(&base) {
            unique.push(base);
        }
    }
    unique
}

/// The nearest directory at or above `path` that holds a `.git` entry.
fn git_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .take_while(|dir| *dir != Path::new("/") && !dir.as_os_str().is_empty())
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// `raw` taken literally: absolute, under `~`, or relative to the first base.
fn literal_path(raw: &str, bases: &[PathBuf], home: Option<&Path>) -> Option<PathBuf> {
    let expanded = expand_tilde(raw, home)?;
    let candidate = if expanded.is_absolute() {
        expanded
    } else {
        bases.first()?.join(expanded)
    };
    candidate.exists().then(|| absolute(&candidate))
}

fn resolve_with_home(raw: &str, bases: &[PathBuf], home: Option<&Path>) -> Option<PathBuf> {
    let expanded = expand_tilde(raw, home)?;
    if expanded.is_absolute() {
        return expanded.exists().then(|| absolute(&expanded));
    }
    let find = |relative: &Path| {
        bases
            .iter()
            .map(|base| base.join(relative))
            .find(|candidate| candidate.exists())
    };
    let stripped = ["a/", "b/", "@"]
        .iter()
        .find_map(|prefix| raw.strip_prefix(prefix))
        .filter(|rest| !rest.is_empty());
    find(&expanded)
        .or_else(|| stripped.and_then(|rest| find(Path::new(rest))))
        .map(|found| absolute(&found))
}

/// Expand a leading `~`. `None` for an empty path, or when there is no home to expand to.
fn expand_tilde(raw: &str, home: Option<&Path>) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    if raw == "~" {
        return home.map(Path::to_path_buf);
    }
    match raw.strip_prefix("~/") {
        Some(rest) => home.map(|home| home.join(rest)),
        None => Some(PathBuf::from(raw)),
    }
}

/// Make a path absolute and remove `.` and `..` components, without touching the disk.
fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str, line: Option<u32>, col: Option<u32>) -> LinkTarget {
        LinkTarget::Path {
            path: p.to_string(),
            line,
            col,
        }
    }

    fn url(u: &str) -> LinkTarget {
        LinkTarget::Url(u.to_string())
    }

    /// Every link in `line` as (linked text, target).
    fn found(line: &str) -> Vec<(String, LinkTarget)> {
        let chars: Vec<char> = line.chars().collect();
        detect_links(line)
            .into_iter()
            .map(|span| (chars[span.start..span.end].iter().collect(), span.target))
            .collect()
    }

    /// Assert that `line` holds exactly one link, covering `text`.
    #[track_caller]
    fn one(line: &str, text: &str, target: LinkTarget) {
        assert_eq!(found(line), vec![(text.to_string(), target)], "in {line:?}");
    }

    #[track_caller]
    fn none(line: &str) {
        assert_eq!(found(line), vec![], "in {line:?}");
    }

    // ---- positions ----------------------------------------------------------------------

    #[test]
    fn paths_with_positions() {
        one(
            "path/to/file.ext:42",
            "path/to/file.ext:42",
            path("path/to/file.ext", Some(42), None),
        );
        one(
            "path/to/file.ext:42:7",
            "path/to/file.ext:42:7",
            path("path/to/file.ext", Some(42), Some(7)),
        );
        one(
            "path/to/file.ext:42-68",
            "path/to/file.ext:42-68",
            path("path/to/file.ext", Some(42), None),
        );
        one(
            "path/to/file.ext:42:7-42:19",
            "path/to/file.ext:42:7-42:19",
            path("path/to/file.ext", Some(42), Some(7)),
        );
        one(
            "path/to/file.ext#L42",
            "path/to/file.ext#L42",
            path("path/to/file.ext", Some(42), None),
        );
        one(
            "path/to/file.ext#L42-L68",
            "path/to/file.ext#L42-L68",
            path("path/to/file.ext", Some(42), None),
        );
        one(
            "path/to/file.ext#L42-68",
            "path/to/file.ext#L42-68",
            path("path/to/file.ext", Some(42), None),
        );
        one(
            "path/to/file.ext#L42C7",
            "path/to/file.ext#L42C7",
            path("path/to/file.ext", Some(42), Some(7)),
        );
        one(
            "path/to/file.ext#L42C7-L43C1",
            "path/to/file.ext#L42C7-L43C1",
            path("path/to/file.ext", Some(42), Some(7)),
        );
        one(
            "~/dir/file.rs:3",
            "~/dir/file.rs:3",
            path("~/dir/file.rs", Some(3), None),
        );
        one(
            "file.ext:12",
            "file.ext:12",
            path("file.ext", Some(12), None),
        );
        one(
            "/abs/path/file.ext:9:1",
            "/abs/path/file.ext:9:1",
            path("/abs/path/file.ext", Some(9), Some(1)),
        );
    }

    #[test]
    fn paths_without_positions() {
        one(
            "/abs/path/file.ext",
            "/abs/path/file.ext",
            path("/abs/path/file.ext", None, None),
        );
        one("./rel/file", "./rel/file", path("./rel/file", None, None));
        one(
            "../rel/file",
            "../rel/file",
            path("../rel/file", None, None),
        );
        one("~/notes", "~/notes", path("~/notes", None, None));
        one("src/term", "src/term", path("src/term", None, None));
        one(
            "crates/term/",
            "crates/term/",
            path("crates/term/", None, None),
        );
        one("README.md", "README.md", path("README.md", None, None));
        one("Cargo.toml", "Cargo.toml", path("Cargo.toml", None, None));
        one("index.d.ts", "index.d.ts", path("index.d.ts", None, None));
        one(".gitignore", ".gitignore", path(".gitignore", None, None));
        one(".env.local", ".env.local", path(".env.local", None, None));
        one(".env", ".env", path(".env", None, None));
        one("Makefile", "Makefile", path("Makefile", None, None));
        one(
            "+page.svelte",
            "+page.svelte",
            path("+page.svelte", None, None),
        );
        one(
            "naïve/café.txt",
            "naïve/café.txt",
            path("naïve/café.txt", None, None),
        );
    }

    #[test]
    fn positions_only_split_off_when_well_formed() {
        assert_eq!(split_position("a.rs"), ("a.rs", None, None));
        assert_eq!(split_position("a.rs:42"), ("a.rs", Some(42), None));
        assert_eq!(split_position("a.rs:42:7"), ("a.rs", Some(42), Some(7)));
        assert_eq!(split_position("a.rs:42-68"), ("a.rs", Some(42), None));
        assert_eq!(split_position("a.rs:42:7-9"), ("a.rs", Some(42), Some(7)));
        assert_eq!(split_position("a.rs:1:2:3"), ("a.rs:1", Some(2), Some(3)));
        assert_eq!(split_position("a.rs#L5"), ("a.rs", Some(5), None));
        assert_eq!(split_position("a.rs#L5-L9"), ("a.rs", Some(5), None));
        assert_eq!(split_position("a.rs#L5C2"), ("a.rs", Some(5), Some(2)));
        // Not positions.
        assert_eq!(split_position("a.rs:"), ("a.rs:", None, None));
        assert_eq!(split_position("a.rs:x"), ("a.rs:x", None, None));
        assert_eq!(split_position("a.rs#L"), ("a.rs#L", None, None));
        assert_eq!(split_position("a.rs#L5x"), ("a.rs#L5x", None, None));
        assert_eq!(split_position("a.rs#Lx"), ("a.rs#Lx", None, None));
        assert_eq!(split_position("a-2"), ("a-2", None, None));
        assert_eq!(split_position("foo-1.2.3"), ("foo-1.2.3", None, None));
        assert_eq!(split_position(":12"), (":12", None, None));
        assert_eq!(split_position("#L12"), ("#L12", None, None));
        assert_eq!(
            split_position("a.rs:99999999999"),
            ("a.rs:99999999999", None, None)
        );
        assert_eq!(split_position(""), ("", None, None));
    }

    // ---- wrappers and punctuation -------------------------------------------------------

    #[test]
    fn wrappers_are_not_part_of_the_link() {
        let want = path("src/main.rs", Some(10), None);
        for line in [
            "`src/main.rs:10`",
            "'src/main.rs:10'",
            "\"src/main.rs:10\"",
            "(src/main.rs:10)",
            "[src/main.rs:10]",
            "<src/main.rs:10>",
            "{src/main.rs:10}",
            "**src/main.rs:10**",
            "see `src/main.rs:10`.",
            "(see src/main.rs:10)",
            "in src/main.rs:10, then",
            "src/main.rs:10;",
            "src/main.rs:10:",
            "src/main.rs:10.",
            "src/main.rs:10!",
            "src/main.rs:10?",
            "src/main.rs:10)",
            "src/main.rs:10]",
            "src/main.rs:10}",
            "src/main.rs:10'",
            "src/main.rs:10\"",
            "│ src/main.rs:10 │",
            "|src/main.rs:10|",
            "┃src/main.rs:10┃",
            "→ src/main.rs:10",
            "• src/main.rs:10",
            "error: src/main.rs:10: oops",
            "\tsrc/main.rs:10\t",
        ] {
            one(line, "src/main.rs:10", want.clone());
        }
    }

    #[test]
    fn trailing_punctuation_without_a_position() {
        one(
            "Look at src/main.rs.",
            "src/main.rs",
            path("src/main.rs", None, None),
        );
        one(
            "Look at src/main.rs...",
            "src/main.rs",
            path("src/main.rs", None, None),
        );
        one(
            "src/main.rs: fn main",
            "src/main.rs",
            path("src/main.rs", None, None),
        );
        one(
            "In main.rs: the entry point",
            "main.rs",
            path("main.rs", None, None),
        );
        one("./src/term:", "./src/term", path("./src/term", None, None));
        // The directory in a shell prompt, without the prompt character.
        one(
            "me@host:~/Code/bakedit$ ls",
            "~/Code/bakedit",
            path("~/Code/bakedit", None, None),
        );
        one(
            "root@host:/etc/nginx# ",
            "/etc/nginx",
            path("/etc/nginx", None, None),
        );
        one(
            "~/Code/bakedit% ",
            "~/Code/bakedit",
            path("~/Code/bakedit", None, None),
        );
        one(
            "edit README.md, then",
            "README.md",
            path("README.md", None, None),
        );
    }

    #[test]
    fn pane_borders_split_tokens() {
        assert_eq!(
            found("src/a.rs:1│src/b.rs:2"),
            vec![
                ("src/a.rs:1".to_string(), path("src/a.rs", Some(1), None)),
                ("src/b.rs:2".to_string(), path("src/b.rs", Some(2), None)),
            ]
        );
        assert_eq!(
            found("a.rs|b.rs"),
            vec![
                ("a.rs".to_string(), path("a.rs", None, None)),
                ("b.rs".to_string(), path("b.rs", None, None))
            ]
        );
        assert_eq!(
            found("─src/a.rs─┬─src/b.rs─"),
            vec![
                ("src/a.rs".to_string(), path("src/a.rs", None, None)),
                ("src/b.rs".to_string(), path("src/b.rs", None, None))
            ]
        );
    }

    #[test]
    fn several_links_on_a_line_keep_their_columns() {
        let line = "é: a.rs:1 and 日本 b/c.rs:22:3";
        let spans = detect_links(line);
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start, spans[0].end), (3, 9));
        assert_eq!(spans[0].target, path("a.rs", Some(1), None));
        assert_eq!((spans[1].start, spans[1].end), (17, 28));
        assert_eq!(spans[1].target, path("b/c.rs", Some(22), Some(3)));
    }

    // ---- compiler and stack trace styles ------------------------------------------------

    #[test]
    fn compiler_and_stack_trace_styles() {
        one(
            "  --> src/lib.rs:10:5",
            "src/lib.rs:10:5",
            path("src/lib.rs", Some(10), Some(5)),
        );
        one(
            "    at f (file.js:10:5)",
            "file.js:10:5",
            path("file.js", Some(10), Some(5)),
        );
        one(
            "    at /srv/app/file.js:10:5",
            "/srv/app/file.js:10:5",
            path("/srv/app/file.js", Some(10), Some(5)),
        );
        one(
            "  File \"x.py\", line 12",
            "x.py",
            path("x.py", Some(12), None),
        );
        one(
            "  File \"/abs/pkg/x.py\", line 12, in <module>",
            "/abs/pkg/x.py",
            path("/abs/pkg/x.py", Some(12), None),
        );
        one(
            "  File \"/Users/me/My Project/x.py\", line 7, in f",
            "/Users/me/My Project/x.py",
            path("/Users/me/My Project/x.py", Some(7), None),
        );
        one(
            "src/app.ts(12,5): error TS2322",
            "src/app.ts(12,5)",
            path("src/app.ts", Some(12), Some(5)),
        );
        one(
            "Program.cs(30): warning",
            "Program.cs(30)",
            path("Program.cs", Some(30), None),
        );
        one(
            "\tat com.acme.Bar.run(Bar.java:42)",
            "Bar.java:42",
            path("Bar.java", Some(42), None),
        );
        one(
            "thread 'main' panicked at src/main.rs:5:9:",
            "src/main.rs:5:9",
            path("src/main.rs", Some(5), Some(9)),
        );
        one(
            "Makefile:12: *** missing separator.  Stop.",
            "Makefile:12",
            path("Makefile", Some(12), None),
        );
        one(
            "PHP Warning: x in /var/www/a.php on line 12",
            "/var/www/a.php",
            path("/var/www/a.php", Some(12), None),
        );
        one(
            "from /srv/a.rb:10:in `call'",
            "/srv/a.rb:10",
            path("/srv/a.rb", Some(10), None),
        );
        one(
            "schema.prisma:14",
            "schema.prisma:14",
            path("schema.prisma", Some(14), None),
        );
        one(
            "weird.ext9:3",
            "weird.ext9:3",
            path("weird.ext9", Some(3), None),
        );
    }

    #[test]
    fn tool_output_styles() {
        one(
            "\tmodified:   src/main.rs",
            "src/main.rs",
            path("src/main.rs", None, None),
        );
        one(
            "+++ b/src/main.rs",
            "b/src/main.rs",
            path("b/src/main.rs", None, None),
        );
        one(
            "⏺ Update(crates/term/src/lib.rs)",
            "crates/term/src/lib.rs",
            path("crates/term/src/lib.rs", None, None),
        );
        one(
            "Read(/abs/file.rs)",
            "/abs/file.rs",
            path("/abs/file.rs", None, None),
        );
        one(
            "--config=conf/app.toml",
            "conf/app.toml",
            path("conf/app.toml", None, None),
        );
        one(
            "OUT=./target/debug",
            "./target/debug",
            path("./target/debug", None, None),
        );
        one(
            "@src/main.rs",
            "@src/main.rs",
            path("@src/main.rs", None, None),
        );
        one(
            "data/year=2024/part.parquet",
            "data/year=2024/part.parquet",
            path("data/year=2024/part.parquet", None, None),
        );
        assert_eq!(
            found("renamed: old.rs -> new.rs"),
            vec![
                ("old.rs".to_string(), path("old.rs", None, None)),
                ("new.rs".to_string(), path("new.rs", None, None))
            ]
        );
        assert_eq!(
            found("PATH=/usr/bin:/bin"),
            vec![
                ("/usr/bin".to_string(), path("/usr/bin", None, None)),
                ("/bin".to_string(), path("/bin", None, None))
            ]
        );
    }

    #[test]
    fn route_segments_keep_their_brackets() {
        one(
            "app/(auth)/login/page.tsx",
            "app/(auth)/login/page.tsx",
            path("app/(auth)/login/page.tsx", None, None),
        );
        one(
            "(see app/(auth)/page.tsx:3)",
            "app/(auth)/page.tsx:3",
            path("app/(auth)/page.tsx", Some(3), None),
        );
        one(
            "pages/blog/[...slug].tsx",
            "pages/blog/[...slug].tsx",
            path("pages/blog/[...slug].tsx", None, None),
        );
        one(
            "app/[[...slug]]/page.tsx",
            "app/[[...slug]]/page.tsx",
            path("app/[[...slug]]/page.tsx", None, None),
        );
        one(
            "[app/[id]/page.tsx]",
            "app/[id]/page.tsx",
            path("app/[id]/page.tsx", None, None),
        );
        // A bracket that never closes is not part of the path.
        one("src/(oops", "src/", path("src/", None, None));
        // Indexing and calls are not paths.
        none("arr[0] items[i] foo(bar)");
    }

    #[test]
    fn quoted_paths_with_spaces() {
        one(
            "open \"~/Library/Application Support/x.json\"",
            "~/Library/Application Support/x.json",
            path("~/Library/Application Support/x.json", None, None),
        );
        one(
            "cd '/Users/me/My Project'",
            "/Users/me/My Project",
            path("/Users/me/My Project", None, None),
        );
        one(
            "`./my dir/a.rs:4`",
            "./my dir/a.rs:4",
            path("./my dir/a.rs", Some(4), None),
        );
        // A quoted command line is tokenised as usual.
        one(
            "\"/usr/bin/env -u X\"",
            "/usr/bin/env",
            path("/usr/bin/env", None, None),
        );
        assert_eq!(found("'/a/b /c/d'").len(), 2);
        // Apostrophes are not quotes around a path.
        one(
            "don't open /tmp/x because it isn't there",
            "/tmp/x",
            path("/tmp/x", None, None),
        );
    }

    // ---- file URLs and Markdown ---------------------------------------------------------

    #[test]
    fn file_urls() {
        one(
            "file:///abs/path#L10",
            "file:///abs/path#L10",
            path("/abs/path", Some(10), None),
        );
        one(
            "file:///abs/path#L10-L20",
            "file:///abs/path#L10-L20",
            path("/abs/path", Some(10), None),
        );
        one(
            "file:///abs/a%20b/c%C3%A9.rs",
            "file:///abs/a%20b/c%C3%A9.rs",
            path("/abs/a b/cé.rs", None, None),
        );
        one(
            "file://localhost/abs/x.rs",
            "file://localhost/abs/x.rs",
            path("/abs/x.rs", None, None),
        );
        one(
            "at file:///srv/app/a.js:10:5",
            "file:///srv/app/a.js:10:5",
            path("/srv/app/a.js", Some(10), Some(5)),
        );
        one(
            "(file:///abs/x.rs).",
            "file:///abs/x.rs",
            path("/abs/x.rs", None, None),
        );
        none("file://");
        none("file://host");
    }

    #[test]
    fn markdown_links_point_at_their_target() {
        one(
            "[label](path/file.rs:10)",
            "[label](path/file.rs:10)",
            path("path/file.rs", Some(10), None),
        );
        one(
            "[label](file:///abs#L10)",
            "[label](file:///abs#L10)",
            path("/abs", Some(10), None),
        );
        one(
            "see [main.rs:42-68](file:///abs/src/main.rs#L42-L68) now",
            "[main.rs:42-68](file:///abs/src/main.rs#L42-L68)",
            path("/abs/src/main.rs", Some(42), None),
        );
        one(
            "[the docs](https://example.com/a_(b))",
            "[the docs](https://example.com/a_(b))",
            url("https://example.com/a_(b)"),
        );
        one(
            "[x](docs/My%20Notes.md)",
            "[x](docs/My%20Notes.md)",
            path("docs/My Notes.md", None, None),
        );
        one("[x](README)", "[x](README)", path("README", None, None));
        one(
            "[x](a.md \"A title\")",
            "[x](a.md \"A title\")",
            path("a.md", None, None),
        );
        one(
            "[x](<my notes/a.md>)",
            "[x](<my notes/a.md>)",
            path("my notes/a.md", None, None),
        );
        one(
            "![shot](img/a.png)",
            "[shot](img/a.png)",
            path("img/a.png", None, None),
        );
        one(
            "[mail](mailto:me@example.com)",
            "[mail](mailto:me@example.com)",
            url("mailto:me@example.com"),
        );
        one(
            "[open](zed://file/abs/x.rs)",
            "[open](zed://file/abs/x.rs)",
            url("zed://file/abs/x.rs"),
        );
        one(
            "[a [nested] label](a.rs)",
            "[a [nested] label](a.rs)",
            path("a.rs", None, None),
        );
    }

    #[test]
    fn things_that_look_like_markdown_but_are_not() {
        // An anchor goes nowhere, but the label can still hold a file.
        none("[section](#heading)");
        one("[main.rs](#L10)", "main.rs", path("main.rs", None, None));
        // Indexing followed by a call.
        none("handlers[name](event)");
        one("[unclosed](a.rs", "a.rs", path("a.rs", None, None));
        one("[note] (a.rs)", "a.rs", path("a.rs", None, None));
    }

    // ---- URLs ---------------------------------------------------------------------------

    #[test]
    fn web_urls() {
        one(
            "https://example.com",
            "https://example.com",
            url("https://example.com"),
        );
        one(
            "http://localhost:3000/a?b=1&c=2#top",
            "http://localhost:3000/a?b=1&c=2#top",
            url("http://localhost:3000/a?b=1&c=2#top"),
        );
        one(
            "HTTPS://EXAMPLE.COM/X",
            "HTTPS://EXAMPLE.COM/X",
            url("HTTPS://EXAMPLE.COM/X"),
        );
        one(
            "http://[::1]:8080/x",
            "http://[::1]:8080/x",
            url("http://[::1]:8080/x"),
        );
        one(
            "url=https://example.com/a",
            "https://example.com/a",
            url("https://example.com/a"),
        );
        one(
            "see:https://example.com/a",
            "https://example.com/a",
            url("https://example.com/a"),
        );
    }

    #[test]
    fn urls_keep_balanced_parentheses_and_drop_trailing_punctuation() {
        let wiki = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        one(wiki, wiki, url(wiki));
        one(&format!("({wiki})"), wiki, url(wiki));
        one(&format!("see {wiki}."), wiki, url(wiki));
        let plain = "https://example.com/a";
        for line in [
            "https://example.com/a.",
            "https://example.com/a,",
            "https://example.com/a;",
            "https://example.com/a:",
            "https://example.com/a!",
            "https://example.com/a?",
            "(https://example.com/a)",
            "(https://example.com/a).",
            "[https://example.com/a]",
            "<https://example.com/a>",
            "\"https://example.com/a\"",
            "'https://example.com/a'",
            "`https://example.com/a`",
            "**https://example.com/a**",
            "https://example.com/a│next pane",
            "https://example.com/a|x",
            "https://example.com/a…",
        ] {
            one(line, plain, url(plain));
        }
        one(
            "https://example.com/a?b=(c)",
            "https://example.com/a?b=(c)",
            url("https://example.com/a?b=(c)"),
        );
    }

    #[test]
    fn other_schemes_are_neither_links_nor_paths() {
        none("ssh://git@github.com/user/repo.git");
        none("postgres://user:pw@db.internal:5432/app");
        none("s3://bucket/key/file.csv");
        none("https://");
        none("http://)");
    }

    // ---- not links ----------------------------------------------------------------------

    #[test]
    fn plain_text_is_not_a_link() {
        for line in [
            "",
            "   ",
            "hello world",
            "The quick brown fox jumps over the lazy dog.",
            "error: mismatched types",
            "Compiling bakedit-term v0.1.0",
            "TODO: fix this",
            "Note: see above",
            "key: value",
            "name=value",
            "a.b.c()",
            "self.title.clone()",
            "console.log(\"x\")",
            "std::env::var",
            "Vec<String>",
            "Option<&str>",
            "user@example.com",
            "example.com",
            "www.example.com:8080",
            "db.internal:5432",
            "localhost:3000",
            "e.g. this, i.e. that, etc.",
            "U.S.A.",
            "-rw-r--r--  1 user  staff  452 Nov 22 19:01",
            "<div></div>",
            "</body>",
            "// a comment",
            "+/-",
            "...",
            "..",
            ".",
            "/",
            "~",
            "\\n",
        ] {
            none(line);
        }
    }

    #[test]
    fn numbers_times_versions_and_addresses_are_not_links() {
        for line in [
            "42",
            "3.14",
            "1,234.56",
            "50%",
            "12:30",
            "12:30:45",
            "9:05am",
            "2026-11-22T10:30:00",
            "2026/11/22",
            "11/22/2026",
            "1/2",
            "24/7",
            "16:9",
            "4:3",
            "1:2:3",
            "v1.2.3",
            "1.2.3",
            "0.26.0",
            "2.0.0-rc.1",
            "1.0.0-alpha",
            "python3.11",
            "1.5x",
            "0.3ms",
            "::1",
            "fe80::1",
            "2001:db8::ff00:42:8329",
            "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
            "aa:bb:cc:dd:ee:ff",
            "127.0.0.1",
            "127.0.0.1:8080",
            "192.168.1.1/24",
        ] {
            none(line);
        }
    }

    #[test]
    fn things_ending_in_a_colon_are_not_links() {
        for line in [
            "error:", "warning:", "Note:", "Usage:", "12:", "a::b:", "http:", "file:",
        ] {
            none(line);
        }
    }

    #[test]
    fn bare_extensions_and_calls_are_not_files() {
        none("all .rs and .toml files");
        none(".then(x)");
        none("main.rs()");
        none("server.log(1, 2)");
    }

    // ---- link_at ------------------------------------------------------------------------

    #[test]
    fn link_at_finds_the_span_under_a_column() {
        let line = "  see src/a.rs:3 ok";
        assert_eq!(link_at(line, 5), None);
        let span = link_at(line, 6).unwrap();
        assert_eq!((span.start, span.end), (6, 16));
        assert_eq!(span.target, path("src/a.rs", Some(3), None));
        assert_eq!(link_at(line, 15).unwrap().start, 6);
        assert_eq!(link_at(line, 16), None);
        assert_eq!(link_at(line, 500), None);
        assert_eq!(link_at("", 0), None);
    }

    #[test]
    fn detection_never_panics_on_odd_input() {
        for line in [
            "[",
            "]",
            "[]",
            "[](",
            "[]()",
            "[x](",
            "[x]()",
            "](",
            "((((",
            "))))",
            "[[[[",
            "]]]]",
            "\"",
            "'",
            "`",
            "\"\"",
            "://",
            "a://",
            "ab://",
            "ab://)",
            ":",
            "::",
            ":1",
            "=",
            "==",
            "=/",
            "a=",
            "#L1",
            "/(",
            "/[",
            "/()",
            "/[]",
            "a/(b",
            "a/[b",
            "a/(b]",
            "a/[b)",
            "%",
            "%%",
            "%zz",
            "file:///%",
            "file:///%E0%A4",
            "\u{301}",
            "a\u{301}/b",
            "🦀/src",
            "\0",
            "\x1b[31m",
            "x\u{7f}y",
            "(1,2)",
            "a.rs(",
            "a.rs(1",
            "a.rs(1,",
            "a.rs(1,2",
            "\", line 1",
            "\" line",
            "- ",
            "--",
            "-=",
            "+=",
            "@",
            "@@",
            "$",
            "$$",
            "~~",
            "~/",
            "./",
            "../",
            "//",
            "///",
            "a//b",
        ] {
            let spans = detect_links(line);
            let len = line.chars().count();
            for span in &spans {
                assert!(
                    span.start < span.end && span.end <= len,
                    "bad span {span:?} in {line:?}"
                );
            }
            for col in 0..=len {
                let _ = link_at(line, col);
            }
        }
        let long = "a/".repeat(5000) + &"[".repeat(5000) + &"(".repeat(5000);
        let _ = detect_links(&long);
    }

    // ---- resolution ---------------------------------------------------------------------

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    /// A temp dir with its symlinks resolved, so results compare equal on macOS.
    fn temp_root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        (dir, root)
    }

    #[test]
    fn resolve_path_tries_each_base_in_order() {
        let (_guard, root) = temp_root();
        touch(&root.join("one/src/a.rs"));
        touch(&root.join("two/src/a.rs"));
        touch(&root.join("two/src/b.rs"));
        let bases = vec![root.join("one"), root.join("two")];
        assert_eq!(
            resolve_path("src/a.rs", &bases),
            Some(root.join("one/src/a.rs"))
        );
        assert_eq!(
            resolve_path("src/b.rs", &bases),
            Some(root.join("two/src/b.rs"))
        );
        assert_eq!(
            resolve_path("./src/b.rs", &bases),
            Some(root.join("two/src/b.rs"))
        );
        assert_eq!(
            resolve_path("src/../src/b.rs", &bases),
            Some(root.join("two/src/b.rs"))
        );
        assert_eq!(resolve_path("src", &bases), Some(root.join("one/src")));
        assert_eq!(resolve_path("src/missing.rs", &bases), None);
        assert_eq!(resolve_path("", &bases), None);
        assert_eq!(resolve_path("src/a.rs", &[]), None);
    }

    #[test]
    fn resolve_path_takes_absolute_paths_as_they_are() {
        let (_guard, root) = temp_root();
        touch(&root.join("x/a.rs"));
        let absolute_path = root.join("x/a.rs");
        let written = absolute_path.to_str().unwrap();
        assert_eq!(resolve_path(written, &[]), Some(absolute_path.clone()));
        assert_eq!(
            resolve_path(written, &[root.join("elsewhere")]),
            Some(absolute_path.clone())
        );
        let dotted = format!("{}/x/../x/./a.rs", root.display());
        assert_eq!(resolve_path(&dotted, &[]), Some(absolute_path));
        assert_eq!(
            resolve_path(
                &format!("{}/x/missing.rs", root.display()),
                std::slice::from_ref(&root)
            ),
            None
        );
    }

    #[test]
    fn tilde_expands_to_home() {
        let (_guard, root) = temp_root();
        touch(&root.join("notes/a.md"));
        let home = Some(root.as_path());
        assert_eq!(
            resolve_with_home("~/notes/a.md", &[], home),
            Some(root.join("notes/a.md"))
        );
        assert_eq!(resolve_with_home("~", &[], home), Some(root.clone()));
        assert_eq!(resolve_with_home("~/missing", &[], home), None);
        assert_eq!(resolve_with_home("~/notes/a.md", &[], None), None);
        // `~user` is not expanded: it is a file name like any other.
        touch(&root.join("~other"));
        assert_eq!(
            resolve_with_home("~other", std::slice::from_ref(&root), home),
            Some(root.join("~other"))
        );
    }

    #[test]
    fn diff_prefixes_and_at_mentions_fall_back_to_the_plain_path() {
        let (_guard, root) = temp_root();
        touch(&root.join("src/a.rs"));
        touch(&root.join("b/real.rs"));
        let bases = vec![root.clone()];
        assert_eq!(
            resolve_path("a/src/a.rs", &bases),
            Some(root.join("src/a.rs"))
        );
        assert_eq!(
            resolve_path("b/src/a.rs", &bases),
            Some(root.join("src/a.rs"))
        );
        assert_eq!(
            resolve_path("@src/a.rs", &bases),
            Some(root.join("src/a.rs"))
        );
        // A real `b/` directory wins over the fallback.
        assert_eq!(
            resolve_path("b/real.rs", &bases),
            Some(root.join("b/real.rs"))
        );
        assert_eq!(resolve_path("a/", &bases), None);
        assert_eq!(resolve_path("@", &bases), None);
    }

    #[test]
    fn targets_resolve_with_their_position() {
        let (_guard, root) = temp_root();
        touch(&root.join("src/a.rs"));
        let bases = vec![root.clone()];
        let resolved = path("src/a.rs", Some(4), Some(2)).resolve(&bases).unwrap();
        assert_eq!(
            resolved,
            ResolvedPath {
                path: root.join("src/a.rs"),
                line: Some(4),
                col: Some(2)
            }
        );
        assert_eq!(path("src/missing.rs", Some(4), None).resolve(&bases), None);
        assert_eq!(url("https://example.com").resolve(&bases), None);
    }

    #[test]
    fn a_file_named_like_a_position_is_kept_whole() {
        let (_guard, root) = temp_root();
        touch(&root.join("notes"));
        touch(&root.join("notes:12"));
        touch(&root.join("log:3:4"));
        touch(&root.join("log"));
        let bases = vec![root.clone()];
        // The detector split "notes:12" into notes + line 12; the file by that very name wins.
        assert_eq!(
            path("notes", Some(12), None).resolve(&bases),
            Some(ResolvedPath {
                path: root.join("notes:12"),
                line: None,
                col: None
            })
        );
        assert_eq!(
            path("notes", Some(13), None).resolve(&bases),
            Some(ResolvedPath {
                path: root.join("notes"),
                line: Some(13),
                col: None
            })
        );
        assert_eq!(
            path("log", Some(3), Some(4)).resolve(&bases),
            Some(ResolvedPath {
                path: root.join("log:3:4"),
                line: None,
                col: None
            })
        );
        // Only the first base counts for this, as the current directory does in zed-show.
        let elsewhere = vec![root.join("nowhere"), root.clone()];
        assert_eq!(
            path("notes", Some(12), None).resolve(&elsewhere),
            Some(ResolvedPath {
                path: root.join("notes"),
                line: Some(12),
                col: None
            })
        );
    }

    #[test]
    fn references_resolve_like_zed_show() {
        let (_guard, root) = temp_root();
        touch(&root.join("src/a b.rs"));
        touch(&root.join("src/a.rs"));
        touch(&root.join("odd:7"));
        let bases = vec![root.clone()];
        let at = |line: Option<u32>, col: Option<u32>| {
            Some(ResolvedPath {
                path: root.join("src/a.rs"),
                line,
                col,
            })
        };
        assert_eq!(resolve_reference("src/a.rs", &bases), at(None, None));
        assert_eq!(resolve_reference("src/a.rs:10", &bases), at(Some(10), None));
        assert_eq!(
            resolve_reference("src/a.rs:10:4", &bases),
            at(Some(10), Some(4))
        );
        assert_eq!(
            resolve_reference("src/a.rs:10-25", &bases),
            at(Some(10), None)
        );
        assert_eq!(
            resolve_reference("src/a.rs#L10-L25", &bases),
            at(Some(10), None)
        );
        assert_eq!(
            resolve_reference("src/a.rs#L10", &bases),
            at(Some(10), None)
        );
        let file_url = format!("file://{}/src/a.rs#L10", root.display());
        assert_eq!(resolve_reference(&file_url, &bases), at(Some(10), None));
        let spaced = format!("file://{}/src/a%20b.rs", root.display());
        assert_eq!(
            resolve_reference(&spaced, &bases),
            Some(ResolvedPath {
                path: root.join("src/a b.rs"),
                line: None,
                col: None
            })
        );
        assert_eq!(
            resolve_reference("odd:7", &bases),
            Some(ResolvedPath {
                path: root.join("odd:7"),
                line: None,
                col: None
            })
        );
        assert_eq!(resolve_reference("src/missing.rs:10", &bases), None);
        assert_eq!(resolve_reference("", &bases), None);
    }

    #[test]
    fn search_bases_follow_zed_show() {
        let (_guard, root) = temp_root();
        let home = root.join("home");
        std::fs::create_dir_all(home.join("Code/zeta/.git")).unwrap();
        std::fs::create_dir_all(home.join("Code/alpha")).unwrap();
        std::fs::create_dir_all(home.join("Code/zeta/crates/term")).unwrap();
        std::fs::write(home.join("Code/notes.txt"), b"x").unwrap();
        let cwd = home.join("Code/zeta/crates/term");
        assert_eq!(
            default_search_bases(&cwd, &home),
            vec![
                cwd.clone(),
                home.join("Code/zeta"),
                home.join("Code"),
                home.join("Code/alpha"),
                home.clone(),
            ]
        );
        // The repository root is not repeated when it is the current directory, and a home
        // with no Code directory still yields its entry.
        assert_eq!(
            default_search_bases(&home.join("Code/zeta"), &home),
            vec![
                home.join("Code/zeta"),
                home.join("Code"),
                home.join("Code/alpha"),
                home.clone()
            ]
        );
        let bare = root.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(
            default_search_bases(&bare, &bare),
            vec![bare.clone(), bare.join("Code")]
        );
    }

    #[test]
    fn a_git_file_marks_a_worktree_root() {
        let (_guard, root) = temp_root();
        std::fs::create_dir_all(root.join("wt/src")).unwrap();
        std::fs::write(root.join("wt/.git"), b"gitdir: elsewhere").unwrap();
        assert_eq!(git_root(&root.join("wt/src")), Some(root.join("wt")));
        assert_eq!(git_root(Path::new("/")), None);
    }
}
