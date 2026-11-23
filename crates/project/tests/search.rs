//! Project search and replace on real directories.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedit_project::search::MAX_LINE_CHARS;
use bakedit_project::{
    replace_all, search, search_with_wake, LineMatch, ReplaceOutcome, ReplaceResult, SearchError,
    SearchEvent, SearchOptions, SkipReason,
};
use common::Project;

const TIMEOUT: Duration = Duration::from_secs(30);

/// Everything one search reported.
#[derive(Debug)]
struct Outcome {
    /// The matches of each file, keyed by its relative path.
    files: BTreeMap<String, Vec<LineMatch>>,
    /// The totals from the final event.
    file_count: usize,
    match_count: usize,
    truncated: bool,
}

impl Outcome {
    /// One line per reported line: `path:line: text` with every match in brackets.
    fn lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (path, matches) in &self.files {
            for found in matches {
                lines.push(format!(
                    "{path}:{}: {}",
                    found.line_number,
                    bracketed(found)
                ));
            }
        }
        lines
    }

    fn paths(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }

    fn reported_matches(&self) -> usize {
        self.files
            .values()
            .flatten()
            .map(|found| found.ranges.len())
            .sum()
    }
}

/// The line's text with `[` and `]` around each match.
fn bracketed(found: &LineMatch) -> String {
    let chars: Vec<char> = found.line.chars().collect();
    let mut text = String::new();
    for (index, c) in chars.iter().enumerate() {
        for range in &found.ranges {
            if range.end == index && range.start != range.end {
                text.push(']');
            }
        }
        for range in &found.ranges {
            if range.start == index {
                text.push('[');
                if range.is_empty() {
                    text.push(']');
                }
            }
        }
        text.push(*c);
    }
    for range in &found.ranges {
        if range.end == chars.len() {
            if range.is_empty() {
                text.push('[');
            }
            text.push(']');
        }
    }
    text
}

/// Run a search to the end and collect what it reported.
fn run(project: &Project, options: SearchOptions) -> Outcome {
    let handle = search(project.root(), options).expect("the search starts");
    let mut files = BTreeMap::new();
    loop {
        match handle
            .events()
            .recv_timeout(TIMEOUT)
            .expect("the search finishes")
        {
            SearchEvent::File { path, matches } => {
                assert!(!matches.is_empty(), "a file event always has matches");
                let previous = files.insert(path.to_string_lossy().into_owned(), matches);
                assert!(previous.is_none(), "each file is reported once");
            }
            SearchEvent::Done {
                files: file_count,
                matches: match_count,
                truncated,
                ..
            } => {
                assert!(
                    handle.events().recv_timeout(TIMEOUT).is_err(),
                    "nothing follows the final event"
                );
                return Outcome {
                    files,
                    file_count,
                    match_count,
                    truncated,
                };
            }
        }
    }
}

fn find(project: &Project, pattern: &str) -> Outcome {
    run(project, SearchOptions::new(pattern))
}

fn regex(pattern: &str) -> SearchOptions {
    SearchOptions {
        regex: true,
        case_sensitive: true,
        ..SearchOptions::new(pattern)
    }
}

#[test]
fn a_literal_search_reports_lines_and_ranges() {
    let project = Project::new();
    project.write(
        "src/main.rs",
        "fn main() {\n    let total = add(1, 2);\n    add(total, 3);\n}\n",
    );
    project.write(
        "src/math.rs",
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    );
    project.write("README.md", "Nothing to see here.\n");

    let outcome = find(&project, "add(");
    assert_eq!(
        outcome.lines(),
        [
            "src/main.rs:2:     let total = [add(]1, 2);",
            "src/main.rs:3:     [add(]total, 3);",
            "src/math.rs:1: pub fn [add(]a: i32, b: i32) -> i32 {",
        ]
    );
    assert_eq!((outcome.file_count, outcome.match_count), (2, 3));
    assert!(!outcome.truncated);

    let first = &outcome.files["src/main.rs"][0];
    assert_eq!(first.line_number, 2);
    assert_eq!(first.line, "    let total = add(1, 2);");
    assert_eq!(first.clamp_offset, 0);
    assert_eq!(first.ranges, vec![16..20]);
}

#[test]
fn several_matches_on_a_line_are_one_line_match() {
    let project = Project::new();
    project.write("a.txt", "one two one\nthree\none\n");
    let outcome = find(&project, "one");
    assert_eq!(
        outcome.lines(),
        ["a.txt:1: [one] two [one]", "a.txt:3: [one]"]
    );
    assert_eq!(outcome.match_count, 3);
    assert_eq!(outcome.files["a.txt"].len(), 2);
}

#[test]
fn text_is_matched_literally_unless_regex_is_on() {
    let project = Project::new();
    project.write("a.txt", "price: $5.00 (approx.)\nprice: 5x00\na.b\naxb\n");
    assert_eq!(
        find(&project, "$5.00 (").lines(),
        ["a.txt:1: price: [$5.00 (]approx.)"]
    );
    assert_eq!(find(&project, "a.b").lines(), ["a.txt:3: [a.b]"]);

    let outcome = run(&project, regex(r"a.b"));
    assert_eq!(outcome.lines(), ["a.txt:3: [a.b]", "a.txt:4: [axb]"]);
    let outcome = run(&project, regex(r"\d+(\.\d+)?"));
    assert_eq!(
        outcome.lines(),
        [
            "a.txt:1: price: $[5.00] (approx.)",
            "a.txt:2: price: [5]x[00]"
        ]
    );
}

#[test]
fn case_is_ignored_unless_asked_for() {
    let project = Project::new();
    project.write("a.txt", "Error: failed\nerror again\nERROR!\n");
    assert_eq!(
        find(&project, "error").lines(),
        [
            "a.txt:1: [Error]: failed",
            "a.txt:2: [error] again",
            "a.txt:3: [ERROR]!"
        ]
    );
    let sensitive = SearchOptions {
        case_sensitive: true,
        ..SearchOptions::new("error")
    };
    assert_eq!(run(&project, sensitive).lines(), ["a.txt:2: [error] again"]);
    let regex_insensitive = SearchOptions {
        regex: true,
        ..SearchOptions::new("^error")
    };
    assert_eq!(run(&project, regex_insensitive).match_count, 3);
}

#[test]
fn whole_word_leaves_longer_words_alone() {
    let project = Project::new();
    project.write(
        "a.txt",
        "cat concat cat_food cat. (cat) Cat\ncatalog\n-cat-\n",
    );
    let whole = SearchOptions {
        whole_word: true,
        case_sensitive: true,
        ..SearchOptions::new("cat")
    };
    assert_eq!(
        run(&project, whole).lines(),
        [
            "a.txt:1: [cat] concat cat_food [cat]. ([cat]) Cat",
            "a.txt:3: -[cat]-"
        ]
    );
    // With a pattern that starts with punctuation, the boundary is at the pattern's edge.
    project.write("b.txt", "x --flag y\nx --flags y\n");
    let flag = SearchOptions {
        whole_word: true,
        ..SearchOptions::new("--flag")
    };
    assert_eq!(run(&project, flag).lines(), ["b.txt:1: x [--flag] y"]);
}

#[test]
fn anchors_work_on_every_line_and_with_windows_line_endings() {
    let project = Project::new();
    project.write("unix.txt", "start middle end\nend start\n");
    project.write("dos.txt", "start middle end\r\nend start\r\nlast end");
    let outcome = run(&project, regex("^start"));
    assert_eq!(
        outcome.lines(),
        [
            "dos.txt:1: [start] middle end",
            "unix.txt:1: [start] middle end"
        ]
    );
    let outcome = run(&project, regex("end$"));
    assert_eq!(
        outcome.lines(),
        [
            "dos.txt:1: start middle [end]",
            "dos.txt:3: last [end]",
            "unix.txt:1: start middle [end]",
        ]
    );
    // The reported text never includes the line break.
    for found in outcome.files.values().flatten() {
        assert!(!found.line.contains('\r') && !found.line.contains('\n'));
    }
}

#[test]
fn patterns_that_match_nothing_report_empty_ranges() {
    let project = Project::new();
    project.write("a.txt", "ab\n\ncd");
    let outcome = run(&project, regex("^"));
    assert_eq!(
        outcome.lines(),
        ["a.txt:1: []ab", "a.txt:2: []", "a.txt:3: []cd"]
    );
    assert_eq!(outcome.match_count, 3);
    let outcome = run(&project, regex("x*$"));
    assert_eq!(outcome.files["a.txt"][0].ranges, vec![2..2]);
}

#[test]
fn bad_input_is_refused_before_anything_starts() {
    let project = Project::with_files(&["a.txt"]);
    let start = |options: SearchOptions| search(project.root(), options).map(|_| ());

    assert!(matches!(
        start(SearchOptions::new("")),
        Err(SearchError::EmptyPattern)
    ));
    let error = start(regex("(unclosed")).unwrap_err();
    assert!(matches!(error, SearchError::InvalidPattern(_)), "{error:?}");
    assert!(error.to_string().contains("unclosed"), "{error}");
    assert!(matches!(
        start(regex("[a-")),
        Err(SearchError::InvalidPattern(_))
    ));
    assert!(matches!(start(regex(r"a\nb")), Err(SearchError::MultiLine)));
    assert!(matches!(
        start(SearchOptions::new("a\nb")),
        Err(SearchError::MultiLine)
    ));

    let bad_glob = SearchOptions {
        include: vec!["src/[".to_string()],
        ..SearchOptions::new("x")
    };
    assert!(matches!(
        start(bad_glob),
        Err(SearchError::InvalidGlob { .. })
    ));
    let bad_exclude = SearchOptions {
        exclude: vec!["{a,b".to_string()],
        ..SearchOptions::new("x")
    };
    assert!(matches!(
        start(bad_exclude),
        Err(SearchError::InvalidGlob { .. })
    ));

    // The same text that is a broken regex is a fine literal.
    assert!(start(SearchOptions::new("(unclosed")).is_ok());
    assert!(matches!(
        replace_all(project.root(), &regex("(unclosed"), "x"),
        Err(SearchError::InvalidPattern(_))
    ));
    assert!(matches!(
        replace_all(project.root(), &SearchOptions::new(""), "x"),
        Err(SearchError::EmptyPattern)
    ));
}

fn needle_project() -> Project {
    let project = Project::new();
    for path in [
        "src/main.rs",
        "src/ui/button.rs",
        "src/ui/button_test.rs",
        "tests/integration.rs",
        "crates/core/src/lib.rs",
        "crates/core/tests/unit.rs",
        "docs/guide.md",
        "Cargo.toml",
        "crates/core/Cargo.toml",
    ] {
        project.write(path, "needle\n");
    }
    project
}

fn globbed(project: &Project, include: &[&str], exclude: &[&str]) -> Vec<String> {
    let owned = |globs: &[&str]| globs.iter().map(|glob| glob.to_string()).collect();
    let options = SearchOptions {
        include: owned(include),
        exclude: owned(exclude),
        ..SearchOptions::new("needle")
    };
    run(project, options)
        .paths()
        .iter()
        .map(|path| path.to_string())
        .collect()
}

#[test]
fn include_globs_pick_files() {
    let project = needle_project();
    assert_eq!(globbed(&project, &[], &[]).len(), 9);
    assert_eq!(globbed(&project, &["*.md"], &[]), ["docs/guide.md"]);
    assert_eq!(
        globbed(&project, &["Cargo.toml"], &[]),
        ["Cargo.toml", "crates/core/Cargo.toml"]
    );
    assert_eq!(globbed(&project, &["/Cargo.toml"], &[]), ["Cargo.toml"]);
    assert_eq!(globbed(&project, &["src/*.rs"], &[]), ["src/main.rs"]);
    assert_eq!(
        globbed(&project, &["src"], &[]),
        [
            "crates/core/src/lib.rs",
            "src/main.rs",
            "src/ui/button.rs",
            "src/ui/button_test.rs",
        ]
    );
    assert_eq!(
        globbed(&project, &["crates/core", "*.md"], &[]),
        [
            "crates/core/Cargo.toml",
            "crates/core/src/lib.rs",
            "crates/core/tests/unit.rs",
            "docs/guide.md",
        ]
    );
    assert!(globbed(&project, &["*.py"], &[]).is_empty());
}

#[test]
fn exclude_globs_drop_files_and_whole_directories() {
    let project = needle_project();
    assert_eq!(
        globbed(&project, &["*.rs"], &["tests", "*_test.rs"]),
        ["crates/core/src/lib.rs", "src/main.rs", "src/ui/button.rs"]
    );
    assert_eq!(
        globbed(&project, &[], &["crates", "src", "tests/"]),
        ["Cargo.toml", "docs/guide.md"]
    );
    assert!(globbed(&project, &["*.md"], &["docs"]).is_empty());
}

#[test]
fn ignored_and_hidden_files_are_left_out() {
    let project = Project::new();
    for path in [
        "src/main.rs",
        "out/generated.rs",
        "cache.tmpdata",
        ".git/config",
        ".git/hooks/pre-commit",
        ".github/workflows/ci.yml",
        ".env",
        "scratch/notes.txt",
    ] {
        project.write(path, "needle\n");
    }
    project.write(".gitignore", "out/\n*.tmpdata\n");
    project.write(".ignore", "scratch/\n");

    assert_eq!(
        find(&project, "needle").paths(),
        [".env", ".github/workflows/ci.yml", "src/main.rs"]
    );
    let everything = SearchOptions {
        include_ignored: true,
        ..SearchOptions::new("needle")
    };
    // Ignored files come back; `.git` never does.
    assert_eq!(
        run(&project, everything).paths(),
        [
            ".env",
            ".github/workflows/ci.yml",
            "cache.tmpdata",
            "out/generated.rs",
            "scratch/notes.txt",
            "src/main.rs",
        ]
    );
}

#[test]
fn binary_files_are_skipped() {
    let project = Project::new();
    project.write("text.txt", "needle in text\n");
    project.write("image.bin", b"needle\x00\x01\x02 needle\n");
    // The NUL comes long after the first matching lines, past the first buffer the
    // file is read in: still binary, still skipped.
    let mut late = "needle early\n".repeat(6_000).into_bytes();
    late.extend_from_slice(b"\x00 needle late\n");
    project.write("late.bin", late);

    let outcome = find(&project, "needle");
    assert_eq!(outcome.lines(), ["text.txt:1: [needle] in text"]);
    assert_eq!((outcome.file_count, outcome.match_count), (1, 1));
}

#[test]
fn other_encodings_are_handled() {
    let project = Project::new();
    // UTF-16 with a byte order mark is decoded.
    let mut utf16 = vec![0xff, 0xfe];
    for unit in "first line\nthe needle here\n".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    project.write("wide.txt", utf16);
    // A UTF-8 byte order mark is not part of the first line.
    project.write("bom.txt", b"\xef\xbb\xbfneedle at the start\n");
    // Bytes that are not UTF-8 show as replacement characters, with ranges that fit.
    project.write("latin1.txt", b"caf\xe9 needle \xe9t\xe9\n");

    let outcome = find(&project, "needle");
    assert_eq!(
        outcome.lines(),
        [
            "bom.txt:1: [needle] at the start",
            "latin1.txt:1: caf\u{fffd} [needle] \u{fffd}t\u{fffd}",
            "wide.txt:2: the [needle] here",
        ]
    );
    assert_eq!(run(&project, regex("^needle")).paths(), ["bom.txt"]);
}

#[test]
fn long_lines_are_clamped_around_their_matches() {
    let project = Project::new();
    let line = format!(
        "{}needle{}needle{}",
        "a".repeat(5_000),
        "b".repeat(5_000),
        "c".repeat(100)
    );
    project.write("min.js", format!("short needle\n{line}\n"));

    let outcome = find(&project, "needle");
    let matches = &outcome.files["min.js"];
    assert_eq!(
        matches.len(),
        3,
        "the short line, then one window per far-apart match"
    );
    assert_eq!(outcome.match_count, 3);

    assert_eq!(matches[0].line, "short needle");
    for (window, position) in matches[1..].iter().zip([5_000, 10_006]) {
        assert_eq!(window.line_number, 2);
        assert_eq!(window.line.chars().count(), MAX_LINE_CHARS);
        assert_eq!(window.ranges.len(), 1);
        assert_eq!(window.clamp_offset + window.ranges[0].start, position);
        let text: String = window
            .line
            .chars()
            .skip(window.ranges[0].start)
            .take(window.ranges[0].len())
            .collect();
        assert_eq!(text, "needle");
    }
}

#[test]
fn the_limit_stops_the_search_and_says_so() {
    let project = Project::new();
    for n in 0..200 {
        project.write(
            &format!("dir{}/file{n}.txt", n % 10),
            "hit\nhit hit\nmiss\nhit\n",
        );
    }
    let total = 200 * 4;

    let all = find(&project, "hit");
    assert_eq!((all.file_count, all.match_count), (200, total));
    assert_eq!(all.reported_matches(), total);
    assert!(!all.truncated);

    let limited = run(
        &project,
        SearchOptions {
            max_results: 25,
            ..SearchOptions::new("hit")
        },
    );
    assert!(limited.truncated);
    assert_eq!(limited.match_count, 25);
    assert_eq!(
        limited.reported_matches(),
        25,
        "exactly the limit is delivered"
    );
    assert_eq!(limited.file_count, limited.files.len());
    assert!(limited.file_count < 200);

    // A limit that is exactly enough is not a truncation.
    let exact = run(
        &project,
        SearchOptions {
            max_results: total,
            ..SearchOptions::new("hit")
        },
    );
    assert!(!exact.truncated);
    assert_eq!(exact.match_count, total);

    let nothing = run(
        &project,
        SearchOptions {
            max_results: 0,
            ..SearchOptions::new("hit")
        },
    );
    assert!(nothing.truncated && nothing.files.is_empty());
    assert_eq!(nothing.match_count, 0);
}

fn big_project() -> Project {
    let project = Project::new();
    let body = "a line with the needle in it\n".repeat(400);
    for n in 0..600 {
        project.write(&format!("pkg{}/file{n}.txt", n % 20), &body);
    }
    project
}

#[test]
fn cancelling_stops_promptly_and_still_finishes() {
    let project = big_project();
    let options = SearchOptions {
        max_results: usize::MAX,
        ..SearchOptions::new("needle")
    };
    let handle = search(project.root(), options).unwrap();
    assert!(!handle.is_cancelled());

    // Let it get going, then stop it.
    let first = handle.events().recv_timeout(TIMEOUT).unwrap();
    assert!(matches!(first, SearchEvent::File { .. }));
    handle.cancel();
    assert!(handle.is_cancelled());
    let cancelled_at = Instant::now();

    let mut files = 1;
    let done = loop {
        match handle
            .events()
            .recv_timeout(TIMEOUT)
            .expect("a final event still arrives")
        {
            SearchEvent::File { .. } => files += 1,
            done @ SearchEvent::Done { .. } => break done,
        }
    };
    assert!(cancelled_at.elapsed() < Duration::from_secs(5));
    let SearchEvent::Done {
        files: counted,
        truncated,
        ..
    } = done
    else {
        unreachable!()
    };
    assert_eq!(counted, files);
    assert!(
        files < 600,
        "the search did not run to the end ({files} files)"
    );
    assert!(
        !truncated,
        "cancelling is not the same as hitting the limit"
    );
}

#[test]
fn dropping_the_handle_cancels_the_search() {
    let project = big_project();
    let options = SearchOptions {
        max_results: usize::MAX,
        ..SearchOptions::new("needle")
    };
    let handle = search(project.root(), options).unwrap();
    let events = handle.events().clone();
    drop(handle);
    // The sender goes away once the workers have stopped: the channel ends long before
    // 600 files' worth of results could have been produced.
    let started = Instant::now();
    let mut files = 0;
    while let Ok(event) = events.recv_timeout(TIMEOUT) {
        if matches!(event, SearchEvent::File { .. }) {
            files += 1;
        }
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(files < 600, "{files} files were still searched");
}

#[test]
fn the_wake_callback_follows_every_event() {
    let project = Project::new();
    for n in 0..5 {
        project.write(&format!("file{n}.txt"), "needle\n");
    }
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let handle = search_with_wake(
        project.root(),
        SearchOptions::new("needle"),
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();
    let mut events = 0;
    while handle.events().recv_timeout(TIMEOUT).is_ok() {
        events += 1;
    }
    assert_eq!(events, 6, "five files and the final event");
    assert_eq!(wakes.load(Ordering::SeqCst), 6);
}

#[test]
fn the_final_event_reports_the_time_taken() {
    let project = Project::with_files(&["a.txt"]);
    let handle = search(project.root(), SearchOptions::new("a.txt")).unwrap();
    let mut elapsed = None;
    while let Ok(event) = handle.events().recv_timeout(TIMEOUT) {
        if let SearchEvent::Done { elapsed: taken, .. } = event {
            elapsed = Some(taken);
        }
    }
    assert!(elapsed.expect("a final event") < TIMEOUT);
}

#[test]
fn a_search_with_no_matches_or_no_directory_just_finishes() {
    let project = Project::with_files(&["a.txt"]);
    let outcome = find(&project, "not in there");
    assert!(outcome.files.is_empty());
    assert_eq!(
        (outcome.file_count, outcome.match_count, outcome.truncated),
        (0, 0, false)
    );

    let missing = search(&project.path("no-such-dir"), SearchOptions::new("x")).unwrap();
    assert!(matches!(
        missing.events().recv_timeout(TIMEOUT),
        Ok(SearchEvent::Done {
            files: 0,
            matches: 0,
            truncated: false,
            ..
        })
    ));
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_not_followed() {
    let project = Project::new();
    project.write("real/file.txt", "needle\n");
    project.symlink("real/file.txt", "alias.txt");
    project.symlink("real", "alias-dir");
    assert_eq!(find(&project, "needle").paths(), ["real/file.txt"]);

    // But the project itself may be reached through a link.
    let link = project.symlink("real", "as-root");
    let handle = search(&link, SearchOptions::new("needle")).unwrap();
    let first = handle.events().recv_timeout(TIMEOUT).unwrap();
    assert!(matches!(first, SearchEvent::File { path, .. } if path == Path::new("file.txt")));
}

fn replaced(results: &[ReplaceResult]) -> Vec<(String, usize)> {
    results
        .iter()
        .map(|result| match result.outcome {
            ReplaceOutcome::Replaced { replacements } => {
                (result.path.to_string_lossy().into_owned(), replacements)
            }
            ref other => panic!("{} was not replaced: {other:?}", result.path.display()),
        })
        .collect()
}

#[test]
fn replace_rewrites_every_match_and_reports_per_file() {
    let project = Project::new();
    project.write("src/b.rs", "let foo = foo(1);\nno match here\nFOO\n");
    project.write("src/a.rs", "foo\n");
    project.write("docs/untouched.md", "nothing\n");

    let results = replace_all(project.root(), &SearchOptions::new("foo"), "bar").unwrap();
    assert_eq!(
        replaced(&results),
        [("src/a.rs".to_string(), 1), ("src/b.rs".to_string(), 3)],
        "sorted by path, only files with matches"
    );
    assert_eq!(project.read("src/a.rs"), "bar\n");
    assert_eq!(
        project.read("src/b.rs"),
        "let bar = bar(1);\nno match here\nbar\n"
    );
    assert_eq!(project.read("docs/untouched.md"), "nothing\n");
    // Nothing is left behind next to the rewritten files.
    assert_eq!(
        project.listing(),
        ["docs/", "docs/untouched.md", "src/", "src/a.rs", "src/b.rs"]
    );

    // Running it again finds nothing.
    assert!(
        replace_all(project.root(), &SearchOptions::new("foo"), "bar")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn replace_respects_case_word_and_literal_settings() {
    let project = Project::new();
    project.write("a.txt", "cat Cat concat cat.\nprice $1 (cat)\n");
    let options = SearchOptions {
        case_sensitive: true,
        whole_word: true,
        ..SearchOptions::new("cat")
    };
    // Without regex the replacement is literal, dollars and all.
    let results = replace_all(project.root(), &options, "$1 dog").unwrap();
    assert_eq!(replaced(&results), [("a.txt".to_string(), 3)]);
    assert_eq!(
        project.read("a.txt"),
        "$1 dog Cat concat $1 dog.\nprice $1 ($1 dog)\n"
    );
}

#[test]
fn replace_expands_capture_groups_in_regex_mode() {
    let project = Project::new();
    project.write("a.rs", "let alpha = 1;\nlet beta = 22;\nconst gamma = 3;\n");
    let results = replace_all(
        project.root(),
        &regex(r"let (\w+) = (?P<value>\d+);"),
        "const ${1}: u8 = $value; // was $$let",
    )
    .unwrap();
    assert_eq!(replaced(&results), [("a.rs".to_string(), 2)]);
    assert_eq!(
        project.read("a.rs"),
        "const alpha: u8 = 1; // was $let\nconst beta: u8 = 22; // was $let\nconst gamma = 3;\n"
    );

    // Anchors and empty matches work line by line.
    project.write("b.txt", "one\ntwo\n\nthree");
    let results = replace_all(project.root(), &regex("^"), "> ").unwrap();
    assert_eq!(
        replaced(&results),
        [("a.rs".to_string(), 3), ("b.txt".to_string(), 4)]
    );
    assert_eq!(project.read("b.txt"), "> one\n> two\n> \n> three");
}

#[test]
fn replace_applies_once_even_when_the_replacement_matches() {
    let project = Project::new();
    project.write("a.txt", "a aa\n");
    let results = replace_all(project.root(), &SearchOptions::new("a"), "aa").unwrap();
    assert_eq!(replaced(&results), [("a.txt".to_string(), 3)]);
    assert_eq!(project.read("a.txt"), "aa aaaa\n");
}

#[test]
fn replace_keeps_line_endings_and_byte_order_marks() {
    let project = Project::new();
    project.write("dos.txt", "old line\r\nkeep\r\nold end");
    project.write("bom.txt", b"\xef\xbb\xbfold first\nold second\n");
    project.write("mixed.txt", "old\nold\r\n\nold");
    let results = replace_all(project.root(), &regex("^old"), "new").unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(
        project.read_bytes("dos.txt"),
        b"new line\r\nkeep\r\nnew end"
    );
    assert_eq!(
        project.read_bytes("bom.txt"),
        b"\xef\xbb\xbfnew first\nnew second\n"
    );
    assert_eq!(project.read_bytes("mixed.txt"), b"new\nnew\r\n\nnew");
}

#[test]
fn replace_leaves_binary_and_wide_files_alone() {
    let project = Project::new();
    project.write("text.txt", "needle\n");
    project.write("binary.bin", b"needle\x00needle\n");
    let mut utf16 = vec![0xff, 0xfe];
    for unit in "the needle\n".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    project.write("wide.txt", &utf16);

    let results = replace_all(project.root(), &SearchOptions::new("needle"), "pin").unwrap();
    assert_eq!(
        results,
        [
            ReplaceResult {
                path: PathBuf::from("text.txt"),
                outcome: ReplaceOutcome::Replaced { replacements: 1 },
            },
            ReplaceResult {
                path: PathBuf::from("wide.txt"),
                outcome: ReplaceOutcome::Skipped(SkipReason::UnsupportedEncoding),
            },
        ]
    );
    assert_eq!(project.read("text.txt"), "pin\n");
    assert_eq!(project.read_bytes("binary.bin"), b"needle\x00needle\n");
    assert_eq!(project.read_bytes("wide.txt"), utf16);
}

#[test]
fn replace_honours_globs_and_ignore_rules_and_not_the_result_limit() {
    let project = Project::new();
    for path in [
        "src/a.rs",
        "src/b.py",
        "out/gen.rs",
        "tests/t.rs",
        ".git/config",
    ] {
        project.write(path, "needle\n");
    }
    project.write(".gitignore", "out/\n");
    project.write("many.rs", "needle\n".repeat(50));

    let options = SearchOptions {
        include: vec!["*.rs".to_string()],
        exclude: vec!["tests".to_string()],
        max_results: 3,
        ..SearchOptions::new("needle")
    };
    let results = replace_all(project.root(), &options, "pin").unwrap();
    assert_eq!(
        replaced(&results),
        [("many.rs".to_string(), 50), ("src/a.rs".to_string(), 1)]
    );
    for untouched in ["src/b.py", "out/gen.rs", "tests/t.rs", ".git/config"] {
        assert_eq!(project.read(untouched), "needle\n", "{untouched}");
    }
}

#[cfg(unix)]
#[test]
fn replace_keeps_permissions_and_does_not_touch_other_files() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::new();
    project.write("run.sh", "#!/bin/sh\necho needle\n");
    project.write("other.txt", "unrelated\n");
    fs::set_permissions(project.path("run.sh"), fs::Permissions::from_mode(0o750)).unwrap();
    let other_before = fs::metadata(project.path("other.txt"))
        .unwrap()
        .modified()
        .unwrap();

    let results = replace_all(project.root(), &SearchOptions::new("needle"), "pin").unwrap();
    assert_eq!(replaced(&results), [("run.sh".to_string(), 1)]);
    assert_eq!(project.read("run.sh"), "#!/bin/sh\necho pin\n");
    let mode = fs::metadata(project.path("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o750);
    let other_after = fs::metadata(project.path("other.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(other_before, other_after);
}

#[cfg(unix)]
#[test]
fn replace_reports_files_it_cannot_write() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::new();
    project.write("locked/file.txt", "needle\n");
    project.write("free/file.txt", "needle\n");
    fs::set_permissions(project.path("locked"), fs::Permissions::from_mode(0o555)).unwrap();

    let results = replace_all(project.root(), &SearchOptions::new("needle"), "pin").unwrap();
    fs::set_permissions(project.path("locked"), fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].path, PathBuf::from("free/file.txt"));
    assert_eq!(
        results[0].outcome,
        ReplaceOutcome::Replaced { replacements: 1 }
    );
    assert_eq!(results[1].path, PathBuf::from("locked/file.txt"));
    assert!(
        matches!(results[1].outcome, ReplaceOutcome::Failed(ref message) if !message.is_empty()),
        "{:?}",
        results[1].outcome
    );
    assert_eq!(
        project.read("locked/file.txt"),
        "needle\n",
        "a failed file is unchanged"
    );
}

#[cfg(unix)]
#[test]
fn replace_does_not_rewrite_a_file_twice_through_a_link() {
    let project = Project::new();
    project.write("real.txt", "a\n");
    project.symlink("real.txt", "link.txt");
    let results = replace_all(project.root(), &SearchOptions::new("a"), "aa").unwrap();
    assert_eq!(replaced(&results), [("real.txt".to_string(), 1)]);
    assert_eq!(project.read("real.txt"), "aa\n");
    assert!(fs::symlink_metadata(project.path("link.txt"))
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn replace_across_many_files_touches_each_exactly_once() {
    let project = Project::new();
    for n in 0..300 {
        project.write(
            &format!("d{}/f{n}.txt", n % 12),
            format!("value = {n}\nvalue again\n"),
        );
    }
    let results = replace_all(project.root(), &SearchOptions::new("value"), "v").unwrap();
    assert_eq!(results.len(), 300);
    assert!(results.windows(2).all(|pair| pair[0].path < pair[1].path));
    assert!(replaced(&results).iter().all(|(_, count)| *count == 2));
    assert_eq!(project.read("d5/f5.txt"), "v = 5\nv again\n");
    assert!(project
        .listing()
        .iter()
        .all(|path| !path.contains(".bakedit-tmp-")));
}
