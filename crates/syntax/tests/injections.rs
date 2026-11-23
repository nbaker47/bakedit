//! Languages inside other languages: each embedded piece is highlighted as itself, and what
//! surrounds it is left as its own language left it.

mod common;

use common::{capture_marked, highlight, render};

/// Highlight `text` as `language` and check each `(piece, capture)`; see `languages.rs`.
fn check(language: &str, text: &str, expectations: &[(&str, &str)]) {
    let spans = highlight(language, text);
    let misses: Vec<String> = expectations
        .iter()
        .filter_map(|(piece, expected)| {
            let actual = capture_marked(text, &spans, piece);
            (actual != *expected)
                .then(|| format!("  {piece:?}: expected {expected:?}, got {actual:?}"))
        })
        .collect();
    assert!(
        misses.is_empty(),
        "{language} is highlighted wrongly:\n{}\n\nall spans:\n{}",
        misses.join("\n"),
        render(text, &spans)
    );
}

#[test]
fn markdown_fences_are_highlighted_in_their_own_language() {
    let text = "\
# Title

```rust
fn main() { let x = \"s\"; }
```

```python
def area(width): return None
```

```js
const f = () => `t`;
```

```sh
echo \"$HOME\" # note
```

```yaml
key: [1, true]
```

```json
{\"k\": null}
```
";
    check(
        "markdown",
        text,
        &[
            ("«fn» main", "keyword"),
            ("fn «main»", "function"),
            ("\"s\"", "string"),
            ("«def» area", "keyword"),
            ("def «area»", "function"),
            ("None", "constant.builtin"),
            ("«const» f", "keyword"),
            ("const «f»", "function"),
            ("`t`", "string"),
            ("«echo» \"", "function"),
            ("# note", "comment"),
            ("«key»:", "property"),
            ("[1, «true»]", "boolean"),
            ("\"k\"", "property.json_key"),
            ("null", "constant.builtin"),
            // The fence itself is still Markdown.
            ("«```»rust", "punctuation.delimiter"),
            ("```«rust»", "string"),
            ("«#» Title", "punctuation.special"),
            ("Title", "title"),
        ],
    );
}

#[test]
fn the_fence_label_is_matched_loosely() {
    for label in [
        "rust",
        "rs",
        "Rust",
        "RUST",
        "rust,ignore",
        "rust no_run",
        "{rust}",
    ] {
        let text = format!("```{label}\nfn main() {{}}\n```\n");
        let spans = highlight("markdown", &text);
        assert_eq!(
            capture_marked(&text, &spans, "«fn» main"),
            "keyword",
            "label {label:?}"
        );
    }
}

#[test]
fn a_fence_in_an_unknown_language_is_left_plain() {
    let text =
        "```klingon\nfn main() {}\n```\n\n```\nno label (at all)\n```\n\n~~~text\ntildes\n~~~\n";
    check(
        "markdown",
        text,
        &[
            ("fn main() {}", "none"),
            ("no label (at all)", "none"),
            ("tildes", "none"),
        ],
    );
}

#[test]
fn a_fence_inside_a_list_or_a_quote_is_highlighted_without_its_prefix() {
    let text = "\
- item

  ```rust
  fn listed() {
      let a = 1;
  }
  ```

> quoted
>
> ```rust
> fn quoted() {
>     let b = 2;
> }
> ```
";
    check(
        "markdown",
        text,
        &[
            ("«fn» listed", "keyword"),
            ("fn «listed»", "function"),
            ("«let» a", "keyword"),
            ("a = «1»", "number"),
            ("«fn» quoted", "keyword"),
            ("fn «quoted»", "function"),
            ("«let» b", "keyword"),
            ("b = «2»", "number"),
            // The quote markers in front of the code are Markdown's, not Rust's.
            ("«> »fn quoted", "punctuation.special"),
            ("«> »    let b", "punctuation.special"),
            ("«> »}", "punctuation.special"),
        ],
    );
}

#[test]
fn markdown_inline_markup_spans_lines_inside_a_quote() {
    let text = "> some *emphasis that\n> continues* here and `code`\n";
    check(
        "markdown",
        text,
        &[
            ("*«emphasis that»", "emphasis"),
            ("«continues»*", "emphasis"),
            ("«> »continues", "punctuation.special"),
            ("`«code»`", "string"),
            ("some ", ""),
        ],
    );
}

#[test]
fn markdown_front_matter_html_and_nested_scripts() {
    let text = "\
---
title: \"Notes\"
draft: true
---

Text with <b>inline html</b>.

<div class=\"box\">
<script>
let answer = 42;
</script>
</div>

+++
key = \"toml\"
+++
";
    check(
        "markdown",
        text,
        &[
            ("«title»:", "property"),
            ("\"Notes\"", "string"),
            ("draft: «true»", "boolean"),
            ("<«b»>", "tag"),
            ("</«b»>", "tag"),
            ("<«div» class", "tag"),
            ("«class»=", "attribute"),
            ("\"«box»\"", "string"),
            // Markdown -> HTML -> JavaScript.
            ("«let» answer", "keyword"),
            ("42", "number"),
            ("inline html", ""),
        ],
    );
}

#[test]
fn html_scripts_and_styles() {
    let text = "\
<html>
<head>
<style>
.card > a:hover { color: red; margin: 0 2px; }
</style>
<script>
  function greet(name) { return `hi ${name}`; }
  const pattern = /a+/;
</script>
<script type=\"module\">import x from \"./x.js\";</script>
</head>
<body onload=\"greet('x')\">text</body>
</html>
";
    check(
        "html",
        text,
        &[
            ("<«style»>", "tag"),
            (".«card»", "property"),
            ("a:«hover»", "attribute"),
            ("«color»:", "property"),
            ("margin: «0»", "number"),
            ("2«px»", "type"),
            ("«function» greet", "keyword"),
            ("function «greet»", "function"),
            ("`hi ", "string"),
            ("«${»name", "punctuation.special"),
            // HTML -> JavaScript -> regex.
            ("/«a»+/", "string"),
            ("/a«+»/", "operator"),
            ("«import» x", "keyword"),
            ("\"./x.js\"", "string"),
            ("<«body» onload", "tag"),
            ("onload", "attribute"),
            (">«text»<", ""),
        ],
    );
}

#[test]
fn rust_macros_nest() {
    let text = "\
fn main() {
    let v = vec![format!(\"{}\", compute(1)), String::from(\"x\")];
    assert_eq!(v.len(), 2, \"message {}\", describe(&v));
    println!(\"{:?}\", matches!(v.first(), Some(s) if s.is_empty()));
}
";
    check(
        "rust",
        text,
        &[
            ("vec!", "function.macro"),
            ("format!", "function.macro"),
            ("«compute»(1)", "function"),
            ("compute(«1»)", "number"),
            ("String::«from»", "function"),
            ("\"x\"", "string"),
            ("assert_eq!", "function.macro"),
            ("v.«len»()", "function.method"),
            ("«describe»(&v)", "function"),
            ("matches!", "function.macro"),
            ("v.«first»()", "function.method"),
            ("s.«is_empty»()", "function.method"),
            ("«if» s.is_empty", "keyword"),
        ],
    );
}

#[test]
fn javascript_tagged_templates_take_the_language_of_their_tag() {
    let text = "\
const style = css`a { color: red; }`;
const page = html`<p class=\"x\">hi</p>`;
const plain = `a { color: red; }`;
const other = unknown`<p>no</p>`;
";
    check(
        "javascript",
        text,
        &[
            ("css`a { «color»: red", "property"),
            ("css`«a» {", "tag"),
            ("html`<«p» class", "tag"),
            ("html`<p «class»", "attribute"),
            // An untagged template and an unknown tag stay strings.
            ("= `«a { color: red; }»`", "string"),
            ("unknown`«<p>no</p>»`", "string"),
        ],
    );
}

#[test]
fn typescript_gets_the_javascript_injections() {
    let text = "const re: RegExp = /x+y/;\nconst s = css`b { top: 0 }`;\n";
    check(
        "typescript",
        text,
        &[
            ("/«x»+y/", "string"),
            ("/x«+»y/", "operator"),
            ("css`«b» {", "tag"),
            ("«top»:", "property"),
        ],
    );
}

#[test]
fn markdown_html_blocks_form_one_document() {
    // The opening and closing tags are separate blocks with Markdown between them.
    let text = "<details>\n<summary>More</summary>\n\nSome *text*.\n\n</details>\n";
    check(
        "markdown",
        text,
        &[
            ("<«details»>", "tag"),
            ("<«summary»>", "tag"),
            ("</«summary»>", "tag"),
            ("</«details»>", "tag"),
            ("*«text»*", "emphasis"),
        ],
    );
}

#[test]
fn markdown_table_cells_hold_inline_markup() {
    let text = "\
| Crate | Job |
|---|---|
| `crates/core` | Shared *types* |
| [text](https://example.com) | **bold** |
";
    check(
        "markdown",
        text,
        &[
            ("`«crates/core»`", "string"),
            ("*«types»*", "emphasis"),
            ("[«text»]", "link_text"),
            ("https://example.com", "link_uri"),
            ("**«bold»**", "emphasis.strong"),
            ("Crate", ""),
            ("Shared ", ""),
        ],
    );
}

#[test]
fn an_injection_that_matches_nothing_usable_leaves_the_host_language_alone() {
    // tree-sitter-php's heredoc injection names the body node, whose text all belongs to
    // its children, so nothing is injected and the heredoc stays a string.
    let text = "<?php\n$q = <<<SQL\nSELECT id FROM users\nSQL;\n";
    check(
        "php",
        text,
        &[("«SELECT» id", "string"), ("«q» =", "variable")],
    );
}

#[test]
fn git_commit_diffs_are_highlighted_as_diffs() {
    let text = "\
fix: a thing

# ------------------------ >8 ------------------------
# Do not modify or remove the line above.
diff --git a/a.txt b/a.txt
index 1111111..2222222 100644
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old line
+new line
";
    check(
        "git_commit",
        text,
        &[
            ("fix", "keyword"),
            (" a thing", "title"),
            ("-old line", "keyword"),
            ("+new line", "string"),
            ("@@ -1 +1 @@", "attribute"),
        ],
    );
}

#[test]
fn deep_nesting_is_cut_off_instead_of_recursing_forever() {
    // Each level of macro nesting is one more layer; forty levels is far past the limit.
    let depth = 40;
    let text = format!(
        "fn main() {{ {}compute(1){}; }}\n",
        "wrap!(".repeat(depth),
        ")".repeat(depth)
    );
    let spans = highlight("rust", &text);
    assert_eq!(capture_marked(&text, &spans, "«fn» main"), "keyword");
    // The outer levels are highlighted as Rust; the innermost are left as the token tree
    // the grammar saw.
    assert_eq!(capture_marked(&text, &spans, "«wrap!»("), "function.macro");
    assert_ne!(capture_marked(&text, &spans, "«compute»(1)"), "function");
    let shallow = "fn main() { wrap!(wrap!(compute(1))); }\n";
    let spans = highlight("rust", shallow);
    assert_eq!(capture_marked(shallow, &spans, "«compute»(1)"), "function");
}

#[test]
fn an_injection_never_changes_what_is_outside_it() {
    // The same Markdown with and without a known fence label: everything outside the fence
    // content is highlighted identically.
    let known = "# T\n\n```rust\nfn f() {}\n```\n\ntail *em*\n";
    let unknown = "# T\n\n```zzzz\nfn f() {}\n```\n\ntail *em*\n";
    let content = known.find("fn f").unwrap()..known.find("```\n\ntail").unwrap();
    let outside = |text: &str| -> Vec<(usize, usize, &'static str)> {
        highlight("markdown", text)
            .into_iter()
            .filter(|span| span.end <= content.start || span.start >= content.end)
            .map(|span| (span.start, span.end, span.capture.name()))
            .collect()
    };
    assert_eq!(outside(known), outside(unknown));
}
