//! Lua patterns, for the `#lua-match?` predicate.
//!
//! Queries written for Neovim test node text with Lua's pattern language instead of a
//! regular expression. The two are close enough that the patterns found in highlight queries
//! translate mechanically; the handful of Lua features with no regular-expression
//! counterpart (`%b`, `%f`, back-references) make the translation fail, and the caller then
//! drops the pattern rather than guess.

use regex::Regex;

/// Compile a Lua pattern into an equivalent regular expression, or `None` if it uses a
/// feature that cannot be translated or is malformed.
pub(crate) fn compile(pattern: &str) -> Option<Regex> {
    Regex::new(&translate(pattern)?).ok()
}

/// The regular expression source for a Lua pattern.
fn translate(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut i = 0;
    if chars.first() == Some(&'^') {
        out.push('^');
        i = 1;
    }
    while i < chars.len() {
        let c = chars[i];
        // What this iteration emitted, when it is a single-character item a quantifier may
        // follow.
        let mut item = true;
        match c {
            '%' => {
                let next = *chars.get(i + 1)?;
                i += 1;
                match next {
                    'b' | 'f' | '0'..='9' => return None,
                    _ if next.is_ascii_alphabetic() => out.push_str(&class(next, false)?),
                    _ => push_literal(&mut out, next),
                }
            }
            '[' => i = translate_set(&chars, i, &mut out)?,
            '.' => out.push_str("(?s:.)"),
            '(' | ')' => {
                out.push(c);
                item = false;
            }
            '$' if i + 1 == chars.len() => {
                out.push('$');
                item = false;
            }
            _ => push_literal(&mut out, c),
        }
        i += 1;
        if item {
            match chars.get(i) {
                Some('*') | Some('+') | Some('?') => {
                    out.push(chars[i]);
                    i += 1;
                }
                Some('-') => {
                    out.push_str("*?");
                    i += 1;
                }
                _ => {}
            }
        }
    }
    Some(out)
}

/// Translate the set starting at `chars[start]` (a `[`) and return the index of its closing
/// `]`.
fn translate_set(chars: &[char], start: usize, out: &mut String) -> Option<usize> {
    let mut i = start + 1;
    out.push('[');
    if chars.get(i) == Some(&'^') {
        out.push('^');
        i += 1;
    }
    let first = i;
    loop {
        let c = *chars.get(i)?;
        match c {
            // A `]` right after the opening is a member of the set, as in Lua.
            ']' if i > first => {
                out.push(']');
                return Some(i);
            }
            '%' => {
                let next = *chars.get(i + 1)?;
                i += 1;
                if next.is_ascii_alphabetic() {
                    out.push_str(&class(next, true)?);
                } else {
                    push_set_literal(out, next);
                }
            }
            // A dash between two members is a range; anywhere else it is itself.
            '-' if i > first && chars.get(i + 1).is_some_and(|&n| n != ']') => out.push('-'),
            _ => push_set_literal(out, c),
        }
        i += 1;
    }
}

/// A Lua character class (`%d`, `%S`, ...) as a regular expression. Inside a set the
/// positive classes are spliced in as members; the negated ones become a nested set.
fn class(letter: char, in_set: bool) -> Option<String> {
    let members = match letter.to_ascii_lowercase() {
        'a' => "A-Za-z",
        'c' => r"\x00-\x1F\x7F",
        'd' => "0-9",
        'g' => "!-~",
        'l' => "a-z",
        'p' => r"!-/:-@\[-`{-~",
        's' => r" \t\n\r\x0B\x0C",
        'u' => "A-Z",
        'w' => "A-Za-z0-9",
        'x' => "A-Fa-f0-9",
        _ => return None,
    };
    Some(match (letter.is_ascii_uppercase(), in_set) {
        (false, true) => members.to_owned(),
        (false, false) => format!("[{members}]"),
        (true, _) => format!("[^{members}]"),
    })
}

/// Append one literal character outside a set.
fn push_literal(out: &mut String, c: char) {
    let mut buffer = [0u8; 4];
    out.push_str(&regex::escape(c.encode_utf8(&mut buffer)));
}

/// Append one literal character inside a set, where a different group of characters is
/// special.
fn push_set_literal(out: &mut String, c: char) {
    if matches!(c, '[' | ']' | '\\' | '^' | '-' | '&' | '~') {
        out.push('\\');
    }
    out.push(c);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, text: &str) -> bool {
        compile(pattern)
            .unwrap_or_else(|| panic!("{pattern:?} should translate"))
            .is_match(text)
    }

    #[test]
    fn the_patterns_the_zig_queries_use() {
        assert!(matches("^[A-Z_][a-zA-Z0-9_]*", "Allocator"));
        assert!(matches("^[A-Z_][a-zA-Z0-9_]*", "_private"));
        assert!(!matches("^[A-Z_][a-zA-Z0-9_]*", "allocator"));
        assert!(matches("^[A-Z][A-Z_0-9]+$", "MAX_SIZE"));
        assert!(!matches("^[A-Z][A-Z_0-9]+$", "MaxSize"));
        assert!(!matches("^[A-Z][A-Z_0-9]+$", "M"));
        assert!(matches("^//!", "//! module docs"));
        assert!(!matches("^//!", "// plain"));
    }

    #[test]
    fn classes_and_their_complements() {
        assert!(matches("^%d+$", "12345"));
        assert!(!matches("^%d+$", "12a45"));
        assert!(matches("^%a%w*$", "x9y"));
        assert!(!matches("^%a%w*$", "9xy"));
        assert!(matches("^%u%l+$", "Hello"));
        assert!(matches("^%s*$", " \t\n"));
        assert!(matches("^%S+$", "word"));
        assert!(!matches("^%S+$", "two words"));
        assert!(matches("^%x+$", "dead00BEEF"));
        assert!(matches("^%p$", "!"));
        assert!(!matches("^%p$", "a"));
        assert!(matches("^[%a_][%w_]*$", "_name1"));
        assert!(matches("^[^%s]+$", "abc"));
        assert!(matches("^[%D]+$", "abc"));
        assert!(!matches("^[%D]+$", "a1c"));
    }

    #[test]
    fn literals_that_are_special_in_a_regex_stay_literal() {
        assert!(matches("a|b", "a|b"));
        assert!(!matches("a|b", "a"));
        assert!(matches("^{x}$", "{x}"));
        assert!(matches("^%.%*%+$", ".*+"));
        assert!(matches("a^b", "a^b"));
        assert!(matches("a$b", "a$b"));
        assert!(matches("^\\d$", "\\d"));
        assert!(matches("^[.]$", "."));
        assert!(!matches("^[.]$", "x"));
        assert!(matches("^[]]$", "]"));
        assert!(matches("^[a%-z]+$", "a-z"));
        assert!(!matches("^[a%-z]+$", "m"));
        assert!(matches("^[+-]$", "-"));
    }

    #[test]
    fn quantifiers_and_anchors() {
        assert!(matches("^a-b$", "aaab"));
        assert!(matches("^a-b$", "b"));
        assert_eq!(
            compile("<.->").unwrap().find("<a><b>").unwrap().as_str(),
            "<a>"
        );
        assert_eq!(
            compile("<.*>").unwrap().find("<a><b>").unwrap().as_str(),
            "<a><b>"
        );
        assert!(matches("colou?r", "color"));
        assert!(matches("x.y", "x\ny"));
        assert!(matches("needle", "hay needle stack"));
        assert!(!matches("^needle", "hay needle stack"));
        assert!(matches("(%a+)=(%d+)", "width=80"));
    }

    #[test]
    fn untranslatable_or_malformed_patterns_are_rejected() {
        assert!(compile("%b()").is_none());
        assert!(compile("%f[%w]").is_none());
        assert!(compile("(a)%1").is_none());
        assert!(compile("trailing%").is_none());
        assert!(compile("[unclosed").is_none());
        assert!(compile("%q").is_none());
    }
}
