//! Speed: highlighting takes time proportional to the text, on real files, on large
//! synthetic ones and on the inputs that make tree-sitter's queries quadratic if they are
//! run naively.
//!
//! The bounds are loose on purpose. They are meant to fail when something goes quadratic,
//! not when the machine is busy: a real regression misses them by a factor of ten or more.
//! Run with `--nocapture` to see the timings, and with `--release` for the numbers that
//! matter.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bakedit_syntax::{
    bracket_marks, languages, outline, HighlightSpan, Highlighter, Language, LineHighlights,
};
use common::language;

/// Whether this is an optimised build, where the strict bounds apply.
const RELEASE: bool = !cfg!(debug_assertions);

/// The fastest of three runs: the least disturbed by whatever else the machine is doing.
fn best_of_three(mut run: impl FnMut() -> usize) -> (Duration, usize) {
    let mut best = Duration::MAX;
    let mut size = 0;
    for _ in 0..3 {
        let started = Instant::now();
        size = run();
        best = best.min(started.elapsed());
    }
    (best, size)
}

/// Time highlighting `text` as `language`; returns the best time and the span count.
fn time_highlight(
    highlighter: &mut Highlighter,
    language: &Language,
    text: &str,
) -> (Duration, usize) {
    best_of_three(|| highlighter.highlight(language, text, None).unwrap().len())
}

/// A Rust source file of about `lines` lines that looks like real code: types, impls,
/// functions, control flow, macros, strings, comments.
fn synthetic_rust(lines: usize) -> String {
    let mut text = String::from("//! A generated module.\nuse std::collections::HashMap;\n\n");
    let mut unit = 0;
    while text.lines().count() < lines {
        text.push_str(&format!(
            r#"/// Record number {unit}.
#[derive(Debug, Clone, PartialEq)]
pub struct Record{unit} {{
    pub id: u64,
    pub name: String,
    tags: Vec<&'static str>,
}}

impl Record{unit} {{
    /// Builds record {unit}.
    pub fn new(id: u64, name: &str) -> Self {{
        let tags = vec!["alpha", "beta", "gamma"];
        Record{unit} {{ id, name: name.to_owned(), tags }}
    }}

    pub fn describe(&self, verbose: bool) -> String {{
        // Not every record has something to say.
        if verbose && !self.tags.is_empty() {{
            format!("{{}} #{{}} [{{}}]", self.name, self.id, self.tags.join(", "))
        }} else {{
            match self.id % 3 {{
                0 => String::from("fizz"),
                1 => self.name.clone(),
                _ => format!("{{:?}}", self.tags.first().copied().unwrap_or("none")),
            }}
        }}
    }}
}}

fn total_{unit}(records: &[Record{unit}], limit: usize) -> u64 {{
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let mut total = 0u64;
    for (index, record) in records.iter().enumerate().take(limit) {{
        *seen.entry(record.id).or_insert(0) += 1;
        total += record.id * 2 + index as u64;
        assert!(total >= record.id, "overflow at {{index}}");
    }}
    println!("total {unit}: {{total}} over {{}} ids", seen.len());
    total
}}

"#
        ));
        unit += 1;
    }
    text
}

/// A JavaScript file of about `lines` lines, heavy on identifiers: the locals query looks
/// every one of them up.
fn synthetic_javascript(lines: usize) -> String {
    let mut text =
        String::from("// A generated module.\nimport { helper } from \"./helper.js\";\n\n");
    let mut unit = 0;
    while text.lines().count() < lines {
        text.push_str(&format!(
            r#"export class Widget{unit} extends Base {{
  constructor(name, options = {{}}) {{
    super(name);
    this.name = name;
    this.options = {{ depth: {unit}, ...options }};
  }}

  render(items, callback) {{
    const prefix = `widget-${{this.name}}-{unit}`;
    const pattern = /^[a-z]+-\d+$/;
    return items
      .filter((item) => pattern.test(item.id) && item.depth <= this.options.depth)
      .map((item, index) => {{
        const label = helper(prefix, item.label ?? "untitled", index);
        callback?.(label);
        return {{ ...item, label, index }};
      }});
  }}
}}

function total{unit}(values, limit) {{
  let total = 0;
  for (const value of values) {{
    if (value > limit) continue;
    total += value * 2;
  }}
  console.log("total {unit}", total);
  return total;
}}

"#
        ));
        unit += 1;
    }
    text
}

/// A Markdown file of about `lines` lines: every paragraph and every fence is its own
/// layer, so this is the stress test for many small injections.
fn synthetic_markdown(lines: usize) -> String {
    let mut text = String::from("# A generated document\n\n");
    let mut unit = 0;
    while text.lines().count() < lines {
        text.push_str(&format!(
            "## Section {unit}\n\nSome *emphasis*, some **strong** text, `code {unit}` and a \
             [link](https://example.com/{unit}).\nA second line with <b>inline html</b>.\n\n\
             - item one\n- item `two`\n  - nested *item*\n\n\
             > A quote with a [reference][ref{unit}].\n\n\
             ```rust\nfn section_{unit}() -> usize {{\n    let value = {unit};\n    value * 2\n}}\n```\n\n\
             | a | b |\n|---|---|\n| 1 | 2 |\n\n[ref{unit}]: https://example.com/ref/{unit}\n\n"
        ));
        unit += 1;
    }
    text
}

/// A Ruby file of about `lines` lines. Ruby decides between "variable" and "method call"
/// from the locals query, the most work the locals machinery does.
fn synthetic_ruby(lines: usize) -> String {
    let mut text = String::from("# A generated module.\nrequire \"json\"\n\n");
    let mut unit = 0;
    while text.lines().count() < lines {
        text.push_str(&format!(
            "class Widget{unit}\n  attr_reader :name, :depth\n\n  def initialize(name, depth = {unit})\n    \
             @name = name\n    @depth = depth\n  end\n\n  def render(items)\n    prefix = \"widget-#{{name}}-{unit}\"\n    \
             items.select {{ |item| item.depth <= depth }}.map do |item|\n      label = [prefix, item.label].join(\"/\")\n      \
             puts label if label.length > 10\n      label\n    end\n  end\nend\n\n"
        ));
        unit += 1;
    }
    text
}

/// Highlight a 5,000 and a 20,000 line version of a language's synthetic file and check
/// both the absolute time and that four times the text takes about four times as long.
fn check_scaling(id: &str, generate: fn(usize) -> String) {
    let language = language(id);
    let mut highlighter = Highlighter::new();
    let (small, large) = (generate(5_000), generate(20_000));
    // Load the grammar and warm the caches before the clock starts.
    highlighter
        .highlight(language, &small[..small.len().min(2_000)], None)
        .unwrap();
    let (small_time, small_spans) = time_highlight(&mut highlighter, language, &small);
    let (large_time, large_spans) = time_highlight(&mut highlighter, language, &large);
    println!(
        "{id}: 5,000 lines ({} kB, {small_spans} spans) in {small_time:.1?}; \
         20,000 lines ({} kB, {large_spans} spans) in {large_time:.1?}",
        small.len() / 1024,
        large.len() / 1024
    );
    assert!(
        small_spans > 10_000 && large_spans > 3 * small_spans,
        "{id}: too few spans"
    );
    let allowed = if RELEASE {
        Duration::from_secs(2)
    } else {
        Duration::from_secs(20)
    };
    assert!(
        large_time < allowed,
        "{id}: 20,000 lines took {large_time:?}"
    );
    // Linear is 4x. Quadratic would be 16x.
    let ratio = large_time.as_secs_f64() / small_time.as_secs_f64().max(1e-6);
    assert!(
        ratio < 9.0,
        "{id}: 4x the text took {ratio:.1}x the time ({small_time:?} -> {large_time:?})"
    );
}

#[test]
fn a_5000_line_rust_file_is_fast() {
    let rust = language("rust");
    let text = synthetic_rust(5_000);
    let mut highlighter = Highlighter::new();
    highlighter
        .highlight(rust, "fn warm_up() {}\n", None)
        .unwrap();
    let (time, spans) = time_highlight(&mut highlighter, rust, &text);
    println!(
        "rust: {} lines, {} kB, {spans} spans in {time:.1?}",
        text.lines().count(),
        text.len() / 1024
    );
    assert!(spans > 30_000);
    // "Well under 100 ms" in a release build; a debug build gets room to be a debug build.
    let allowed = if RELEASE {
        Duration::from_millis(100)
    } else {
        Duration::from_millis(2_500)
    };
    assert!(time < allowed, "5,000 lines of Rust took {time:?}");
}

#[test]
fn rust_scales_linearly_to_20000_lines() {
    check_scaling("rust", synthetic_rust);
}

#[test]
fn javascript_with_its_locals_scales_linearly() {
    check_scaling("javascript", synthetic_javascript);
}

#[test]
fn typescript_scales_linearly() {
    // The same text is valid TypeScript, and its query is JavaScript's plus more.
    check_scaling("typescript", synthetic_javascript);
}

#[test]
fn ruby_with_its_locals_scales_linearly() {
    check_scaling("ruby", synthetic_ruby);
}

#[test]
fn markdown_with_thousands_of_injected_layers_scales_linearly() {
    check_scaling("markdown", synthetic_markdown);
}

/// The largest `.rs` files under the workspace's `crates` directory.
fn largest_sources(count: usize) -> Vec<(PathBuf, String)> {
    fn collect(directory: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut paths = Vec::new();
    collect(&crates, &mut paths);
    let mut sources: Vec<(PathBuf, String)> = paths
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(&path).ok().map(|text| (path, text)))
        .collect();
    sources.sort_by_key(|(_, text)| std::cmp::Reverse(text.len()));
    sources.truncate(count);
    sources
}

#[test]
fn this_repositorys_largest_sources_are_fast() {
    let rust = language("rust");
    let mut highlighter = Highlighter::new();
    highlighter
        .highlight(rust, "fn warm_up() {}\n", None)
        .unwrap();
    let sources = largest_sources(8);
    assert!(
        sources.len() >= 4,
        "found only {} Rust sources",
        sources.len()
    );
    for (path, text) in &sources {
        let (time, spans) = time_highlight(&mut highlighter, rust, text);
        let lines = text.lines().count();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?");
        println!(
            "{name}: {lines} lines, {} kB, {spans} spans in {time:.1?}",
            text.len() / 1024
        );
        assert!(spans > lines, "{name}: {spans} spans for {lines} lines");
        // A couple of thousand lines: a tenth of a second would already be far too slow.
        let allowed = if RELEASE {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(1_500)
        };
        assert!(time < allowed, "{name} took {time:?}");
        // The rest of what the worker does with a file, and the outline.
        let spans = highlighter.highlight(rust, text, None).unwrap();
        let (rest, _) = best_of_three(|| {
            LineHighlights::new(text, &spans).line_count() + bracket_marks(text, &spans).len()
        });
        let (outlined, symbols) = best_of_three(|| outline(rust, text).len());
        println!(
            "  lines and brackets in {rest:.1?}; outline of {symbols} symbols in {outlined:.1?}"
        );
        assert!(
            rest < allowed && outlined < allowed,
            "{name}: {rest:?} and {outlined:?}"
        );
        assert!(symbols > 0, "{name} has no outline");
    }
}

/// Inputs that nest thousands of levels deep, or that a grammar reads as such. Each is what
/// a file of generated or minified code, or a file opened as the wrong language, looks like
/// to a query: long chains that keep matches open.
fn degenerate_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("method chain", format!("x = a{};\n", ".b()".repeat(6_000))),
        (
            "operator chain",
            format!("x = {}a;\n", "a + ".repeat(12_000)),
        ),
        (
            "nested calls",
            format!("x = {}1{};\n", "f(".repeat(4_000), ")".repeat(4_000)),
        ),
        (
            "nested brackets",
            format!("{}{}\n", "[{(".repeat(1_500), ")}]".repeat(1_500)),
        ),
        ("statements on one line", "x = 1; ".repeat(12_000)),
        ("unclosed everything", "( [ { \" ' ` /* <!-- ".repeat(800)),
        (
            "else-if chain",
            format!(
                "if (a) {{ b(); }} {}\n",
                "else if (a) { b(); } ".repeat(3_000)
            ),
        ),
    ]
}

#[test]
fn degenerate_inputs_take_bounded_time_in_every_language() {
    // Before the highlighter bounded its queries, tree-sitter-nix took most of a minute on
    // one of these and a dozen grammars took several seconds on another. What is left is
    // the parsers: a few are slow on garbage, and the slowest are stopped after two seconds.
    let allowed = Duration::from_secs(12);
    let inputs = degenerate_inputs();
    let mut highlighter = Highlighter::new();
    let mut slowest = (Duration::ZERO, String::new());
    let suite = Instant::now();
    for language in languages().iter().filter(|language| language.has_grammar()) {
        for (name, text) in &inputs {
            let started = Instant::now();
            let spans = highlighter.highlight(language, text, None).unwrap();
            let highlighted = started.elapsed();
            let _ = (
                LineHighlights::new(text, &spans),
                bracket_marks(text, &spans),
            );
            let _ = outline(language, text);
            let total = started.elapsed();
            assert!(
                total < allowed,
                "{name} as {}: {total:?} (highlight {highlighted:?})",
                language.id
            );
            if total > slowest.0 {
                slowest = (total, format!("{name} as {}", language.id));
            }
        }
    }
    println!(
        "slowest: {} in {:.1?}; all of them in {:.1?}",
        slowest.1,
        slowest.0,
        suite.elapsed()
    );
}

#[test]
fn deep_code_is_still_highlighted_all_the_way_down() {
    // The bound on query time must not cost the deep end of a tree its colours.
    let captured = |spans: &[HighlightSpan], name: &str| {
        spans
            .iter()
            .filter(|span| span.capture.name() == name)
            .count()
    };

    // 3,000 chained method calls: every one is a method call.
    let chain = format!("fn f() {{ let x = a{}; }}\n", ".method()".repeat(3_000));
    let spans = common::highlight("rust", &chain);
    assert_eq!(captured(&spans, "function.method"), 3_000);

    // 1,500 nested arrays, a number on each level.
    let nested = format!(
        "const x = {}0{};\n",
        "[1, ".repeat(1_500),
        "]".repeat(1_500)
    );
    let spans = common::highlight("javascript", &nested);
    assert_eq!(captured(&spans, "number"), 1_501);

    // 1,000 nested objects: every key is a key and the value at the bottom is a string.
    let json = format!(
        "{}\"bottom\"{}\n",
        "{\"key\": ".repeat(1_000),
        "}".repeat(1_000)
    );
    let spans = common::highlight("json", &json);
    assert_eq!(captured(&spans, "property.json_key"), 1_000);
    assert_eq!(
        common::capture_marked(&json, &spans, "\"bottom\""),
        "string"
    );

    // 4,000 terms: tree-sitter-lua captures an operator through its parent expression,
    // which is the kind of pattern that used to be quadratic.
    let sum = format!("local x = {}1\n", "y + ".repeat(4_000));
    let spans = common::highlight("lua", &sum);
    assert_eq!(captured(&spans, "operator"), 4_001);

    // An injection at the bottom of 700 nested calls is still found.
    let buried = format!(
        "fn f() {{ {}vec![compute(1)]{}; }}\n",
        "wrap(".repeat(700),
        ")".repeat(700)
    );
    let spans = common::highlight("rust", &buried);
    assert_eq!(
        common::capture_marked(&buried, &spans, "«compute»(1)"),
        "function"
    );
    assert_eq!(captured(&spans, "function"), 702);
}

#[test]
fn locals_resolve_across_a_deep_tree() {
    // 200 functions nested in each other, each using every enclosing parameter.
    let depth = 200;
    let mut text = String::new();
    for level in 0..depth {
        text.push_str(&format!("function f{level}(p{level}: number) {{\n"));
    }
    text.push_str("return ");
    text.push_str(
        &(0..depth)
            .map(|level| format!("p{level}"))
            .collect::<Vec<_>>()
            .join(" + "),
    );
    text.push_str(";\n");
    text.push_str(&"}\n".repeat(depth));
    let spans = common::highlight("typescript", &text);
    for level in [0, 1, 57, 128, 199] {
        let definition = format!("(«p{level}»: number)");
        assert_eq!(
            common::capture_marked(&text, &spans, &definition),
            "variable.parameter"
        );
        // The reference is deeper than the definition by up to 400 levels.
        let reference = format!(" «p{level}»{}", if level + 1 == depth { ";" } else { " +" });
        let start = text.find("return ").unwrap();
        let offset = start
            + text[start..]
                .find(reference.replace(['«', '»'], "").as_str())
                .unwrap()
            + 1;
        let name = format!("p{level}");
        assert_eq!(
            common::capture_of_range(&text, &spans, offset, offset + name.len()),
            "variable.parameter",
            "the reference to {name}"
        );
    }
}
