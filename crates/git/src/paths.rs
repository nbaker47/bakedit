//! Paths as git sees them: plain bytes, relative to the top of the work tree.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

/// The path git wrote as `bytes`. Nothing is decoded: a name that is not valid UTF-8
/// stays exactly as it is on disk.
pub(crate) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(bytes))
}

/// The bytes of `path` as they are handed to git.
pub(crate) fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

/// A pathspec that matches `path` (relative to the top of the work tree) and nothing
/// else: no globbing, no pathspec magic, whatever characters the name contains. The empty
/// path stands for the whole work tree.
pub(crate) fn literal_pathspec(path: &Path) -> OsString {
    let mut spec = b":(literal)".to_vec();
    if path.as_os_str().is_empty() {
        spec.push(b'.');
    } else {
        spec.extend_from_slice(path_bytes(path));
    }
    OsString::from_vec(spec)
}

/// The standard input for `--pathspec-from-file=- --pathspec-file-nul`: one literal
/// pathspec per path, each followed by a NUL.
pub(crate) fn pathspec_input(paths: &[PathBuf]) -> Vec<u8> {
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(literal_pathspec(path).as_bytes());
        input.push(0);
    }
    input
}

/// `path` with `.` components dropped and `..` components resolved, or `None` when it is
/// absolute or climbs above its starting point.
pub(crate) fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => clean.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                if !clean.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(clean)
}

/// Reads a C-style quoted string, the form git uses for unusual file names, from the
/// start of `text` (which must begin with the opening quote). Returns the bytes it stands
/// for and how many bytes of `text` it took up, closing quote included. `None` when the
/// quoting is malformed.
pub(crate) fn unquote_c_style(text: &[u8]) -> Option<(Vec<u8>, usize)> {
    if text.first() != Some(&b'"') {
        return None;
    }
    let mut bytes = Vec::new();
    let mut at = 1;
    loop {
        match *text.get(at)? {
            b'"' => return Some((bytes, at + 1)),
            b'\\' => {
                let escape = *text.get(at + 1)?;
                at += 2;
                bytes.push(match escape {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'v' => 0x0b,
                    b'\\' | b'"' => escape,
                    b'0'..=b'3' => {
                        let second = octal_digit(*text.get(at)?)?;
                        let third = octal_digit(*text.get(at + 1)?)?;
                        at += 2;
                        ((escape - b'0') << 6) | (second << 3) | third
                    }
                    _ => return None,
                });
            }
            byte => {
                bytes.push(byte);
                at += 1;
            }
        }
    }
}

fn octal_digit(byte: u8) -> Option<u8> {
    matches!(byte, b'0'..=b'7').then(|| byte - b'0')
}

/// Whether git writes `name` in quotes in a patch header (with `core.quotepath` off):
/// it does when the name holds a control character, a double quote or a backslash.
pub(crate) fn needs_quoting(name: &[u8]) -> bool {
    name.iter()
        .any(|&byte| byte < 0x20 || byte == 0x7f || byte == b'"' || byte == b'\\')
}

/// `name` in git's C-style quotes. Bytes above ASCII are kept as they are, which is what
/// git does with `core.quotepath` off and what it accepts back in a patch.
pub(crate) fn quote_c_style(name: &[u8]) -> Vec<u8> {
    let mut quoted = Vec::with_capacity(name.len() + 2);
    quoted.push(b'"');
    for &byte in name {
        match byte {
            0x07 => quoted.extend_from_slice(b"\\a"),
            0x08 => quoted.extend_from_slice(b"\\b"),
            0x0c => quoted.extend_from_slice(b"\\f"),
            b'\n' => quoted.extend_from_slice(b"\\n"),
            b'\r' => quoted.extend_from_slice(b"\\r"),
            b'\t' => quoted.extend_from_slice(b"\\t"),
            0x0b => quoted.extend_from_slice(b"\\v"),
            b'\\' => quoted.extend_from_slice(b"\\\\"),
            b'"' => quoted.extend_from_slice(b"\\\""),
            byte if byte < 0x20 || byte == 0x7f => {
                quoted.push(b'\\');
                quoted.push(b'0' + (byte >> 6));
                quoted.push(b'0' + ((byte >> 3) & 7));
                quoted.push(b'0' + (byte & 7));
            }
            byte => quoted.push(byte),
        }
    }
    quoted.push(b'"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip_without_decoding() {
        let raw = b"dir/caf\xe9 \xff.txt";
        let path = path_from_bytes(raw);
        assert_eq!(path_bytes(&path), raw);
        assert!(path.to_str().is_none(), "not valid UTF-8, yet kept");
    }

    #[test]
    fn literal_pathspecs_switch_off_globbing_and_magic() {
        assert_eq!(
            literal_pathspec(Path::new("app/[id]/page.tsx")).as_bytes(),
            b":(literal)app/[id]/page.tsx"
        );
        assert_eq!(
            literal_pathspec(Path::new(":colon*")).as_bytes(),
            b":(literal):colon*"
        );
        assert_eq!(literal_pathspec(Path::new("")).as_bytes(), b":(literal).");
    }

    #[test]
    fn pathspec_input_is_nul_terminated() {
        let paths = [PathBuf::from("a b.txt"), PathBuf::from("new\nline")];
        assert_eq!(
            pathspec_input(&paths),
            b":(literal)a b.txt\0:(literal)new\nline\0"
        );
        assert!(pathspec_input(&[]).is_empty());
    }

    #[test]
    fn relative_paths_are_normalized() {
        let clean = |text: &str| normalize_relative(Path::new(text));
        assert_eq!(clean("a/b.txt"), Some(PathBuf::from("a/b.txt")));
        assert_eq!(clean("./a//b/../c.txt"), Some(PathBuf::from("a/c.txt")));
        assert_eq!(clean("."), Some(PathBuf::new()));
        assert_eq!(clean(""), Some(PathBuf::new()));
        assert_eq!(clean("a/.."), Some(PathBuf::new()));
        assert_eq!(clean("../a"), None);
        assert_eq!(clean("a/../../b"), None);
        assert_eq!(clean("/abs"), None);
    }

    #[test]
    fn unquotes_what_git_quotes() {
        let (name, used) = unquote_c_style(br#""a/ta\tb.txt" rest"#).unwrap();
        assert_eq!(name, b"a/ta\tb.txt");
        assert_eq!(used, 13);
        let (name, _) = unquote_c_style(br#""quo\"te\\back\nline""#).unwrap();
        assert_eq!(name, b"quo\"te\\back\nline");
        let (name, _) = unquote_c_style(br#""\303\274 \001\a\b\f\r\v""#).unwrap();
        assert_eq!(name, b"\xc3\xbc \x01\x07\x08\x0c\r\x0b");
        // Raw bytes above ASCII are what core.quotepath=off produces.
        let (name, _) = unquote_c_style(b"\"caf\xc3\xa9\\t\"").unwrap();
        assert_eq!(name, b"caf\xc3\xa9\t");
    }

    #[test]
    fn malformed_quoting_is_rejected() {
        assert_eq!(unquote_c_style(b"plain"), None);
        assert_eq!(unquote_c_style(b"\"no end"), None);
        assert_eq!(unquote_c_style(b"\"bad \\q\""), None);
        assert_eq!(unquote_c_style(b"\"short \\30\""), None);
        assert_eq!(unquote_c_style(b"\"big \\777\""), None);
        assert_eq!(unquote_c_style(b"\"dangling \\"), None);
        assert_eq!(unquote_c_style(b""), None);
    }

    #[test]
    fn quoting_matches_git_and_round_trips() {
        assert!(!needs_quoting(b"dir with space/\xc3\xbc.txt"));
        assert!(needs_quoting(b"ta\tb"));
        assert!(needs_quoting(b"quo\"te"));
        assert!(needs_quoting(b"back\\slash"));
        assert!(needs_quoting(b"del\x7f"));
        assert_eq!(quote_c_style(b"a/ta\tb.txt"), br#""a/ta\tb.txt""#);
        assert_eq!(quote_c_style(b"new\nline"), br#""new\nline""#);
        assert_eq!(quote_c_style(b"q\"b\\"), br#""q\"b\\""#);
        assert_eq!(quote_c_style(b"\x01\x7f"), br#""\001\177""#);

        let every_byte: Vec<u8> = (1..=255).collect();
        let quoted = quote_c_style(&every_byte);
        let (back, used) = unquote_c_style(&quoted).unwrap();
        assert_eq!(back, every_byte);
        assert_eq!(used, quoted.len());
    }
}
