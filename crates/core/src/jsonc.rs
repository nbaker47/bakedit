//! JSON with comments and trailing commas, the dialect Zed's settings and keymap files use.

use serde::de::DeserializeOwned;

/// Replace `//` and `/* */` comments with spaces and drop trailing commas, leaving plain
/// JSON. Comment bytes become spaces (newlines are kept) so error positions still line up.
pub fn strip(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b);
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                out.push(b);
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    out.push(b' ');
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                out.extend_from_slice(b"  ");
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
                if i < bytes.len() {
                    out.extend_from_slice(b"  ");
                    i += 2;
                }
            }
            b',' => {
                // A comma is trailing when the next thing that is not whitespace or a comment
                // closes the container.
                let mut j = i + 1;
                loop {
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'/') && bytes.get(j + 1) == Some(&b'/') {
                        while j < bytes.len() && bytes[j] != b'\n' {
                            j += 1;
                        }
                    } else if bytes.get(j) == Some(&b'/') && bytes.get(j + 1) == Some(&b'*') {
                        j += 2;
                        while j < bytes.len() && !(bytes[j] == b'*' && bytes.get(j + 1) == Some(&b'/')) {
                            j += 1;
                        }
                        j = (j + 2).min(bytes.len());
                    } else {
                        break;
                    }
                }
                let trailing = matches!(bytes.get(j), Some(b'}') | Some(b']'));
                out.push(if trailing { b' ' } else { b',' });
                i += 1;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    // Only ASCII bytes were replaced, and only outside strings, so this is still UTF-8.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Parse JSON-with-comments into any deserialisable type.
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(&strip(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn strips_comments_and_trailing_commas() {
        let text = r#"
        // a comment
        {
          "a": 1, // trailing comment
          /* block
             comment */
          "b": [1, 2, 3,],
          "c": {"d": "e",},
        }
        "#;
        let value: Value = from_str(text).unwrap();
        assert_eq!(value, json!({"a": 1, "b": [1, 2, 3], "c": {"d": "e"}}));
    }

    #[test]
    fn leaves_string_contents_alone() {
        let text = r#"{"url": "https://example.com/a,]", "s": "say \"hi\" // not a comment", "t": "/* no */"}"#;
        let value: Value = from_str(text).unwrap();
        assert_eq!(value["url"], "https://example.com/a,]");
        assert_eq!(value["s"], "say \"hi\" // not a comment");
        assert_eq!(value["t"], "/* no */");
    }

    #[test]
    fn trailing_comma_before_a_comment() {
        let text = "[\n  1,\n  2, // last\n]";
        let value: Value = from_str(text).unwrap();
        assert_eq!(value, json!([1, 2]));
        let text = "{\"a\": 1, /* c */ }";
        let value: Value = from_str(text).unwrap();
        assert_eq!(value, json!({"a": 1}));
    }

    #[test]
    fn keeps_line_numbers() {
        let text = "{\n// one\n/* two\nthree */\n\"a\": }";
        let stripped = strip(text);
        assert_eq!(stripped.matches('\n').count(), text.matches('\n').count());
        let err = serde_json::from_str::<Value>(&stripped).unwrap_err();
        assert_eq!(err.line(), 5);
    }

    #[test]
    fn keeps_non_ascii() {
        let value: Value = from_str("{\"名前\": \"房產金管家\", // コメント\n}").unwrap();
        assert_eq!(value["名前"], "房產金管家");
    }

    #[test]
    fn parses_the_real_zed_files_shape() {
        let text = r#"[
          {
            "context": "Workspace",
            "bindings": {
              // "shift shift": "file_finder::Toggle"
              "cmd-t": "terminal_panel::Toggle",
              "cmd-alt-g": ["action::Sequence", ["git::Diff", "workspace::ToggleZoom"]],
            },
          },
        ]"#;
        let value: Value = from_str(text).unwrap();
        assert_eq!(value[0]["bindings"]["cmd-t"], "terminal_panel::Toggle");
    }
}
