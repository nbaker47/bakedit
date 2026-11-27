//! Format on save, by running the project's own formatter over the buffer text.
//!
//! bakedit has no language servers, so formatting means an external command that reads the
//! file on stdin and writes the result to stdout. A formatter is only chosen when the
//! project is visibly set up for it (a `Cargo.toml`, a prettier config, a ruff section...),
//! the same caution Zed shows: saving a file must never reformat a project that did not ask.
//! `"formatters"` in settings names a command per language and always wins.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a formatter may take before it is stopped and the save goes ahead unformatted.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Why a formatter run produced nothing usable. The save still goes ahead, unformatted.
#[derive(Debug)]
pub enum FormatError {
    /// The program could not be started.
    Spawn { program: String, source: std::io::Error },
    /// It ran past the timeout and was stopped.
    TimedOut { program: String },
    /// It exited with an error; `message` is the first line it printed to stderr.
    Failed { program: String, message: String },
    /// It succeeded but printed nothing for a non-empty file.
    EmptyOutput { program: String },
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Spawn { program, source } => write!(f, "could not run {program}: {source}"),
            FormatError::TimedOut { program } => write!(f, "{program} took too long and was stopped"),
            FormatError::Failed { program, message } => write!(f, "{program}: {message}"),
            FormatError::EmptyOutput { program } => write!(f, "{program} printed nothing; file left as it was"),
        }
    }
}

impl std::error::Error for FormatError {}

/// Find `name` on `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        if let Some(found) = std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| is_executable(candidate)) {
            return Some(found);
        }
    }
    // GUI-launched macOS apps (Kitty, Ghostty, Finder) inherit a minimal PATH without Homebrew
    // or user-local bin dirs; check standard locations as a fallback.
    let mut fallbacks: Vec<PathBuf> = vec![PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")];
    if let Some(home) = dirs::home_dir() {
        fallbacks.push(home.join(".local/bin"));
        fallbacks.push(home.join(".cargo/bin"));
    }
    fallbacks.into_iter().map(|dir| dir.join(name)).find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// The directories from the file's folder up to (and including) the project root. If the
/// file is outside the root, just its own ancestors up to the filesystem root.
fn ancestors_within<'a>(file: &'a Path, root: &'a Path) -> impl Iterator<Item = &'a Path> {
    let inside = file.starts_with(root);
    file.ancestors().skip(1).take_while(move |dir| !inside || dir.starts_with(root))
}

fn find_up(file: &Path, root: &Path, names: &[&str]) -> Option<PathBuf> {
    ancestors_within(file, root).find_map(|dir| names.iter().map(|name| dir.join(name)).find(|p| p.exists()))
}

fn file_contains(path: &Path, needle: &str) -> bool {
    std::fs::read_to_string(path).map(|text| text.contains(needle)).unwrap_or(false)
}

/// The Rust edition for a file: from the nearest `Cargo.toml`, following
/// `edition.workspace = true` up to the workspace manifest. 2021 when nothing says.
fn rust_edition(file: &Path, root: &Path) -> String {
    let mut inherit = false;
    for dir in ancestors_within(file, root) {
        let Ok(manifest) = std::fs::read_to_string(dir.join("Cargo.toml")) else { continue };
        for line in manifest.lines() {
            let line = line.trim();
            if line.starts_with("edition.workspace") || (line.starts_with("edition") && line.contains("workspace = true")) {
                inherit = true;
                continue;
            }
            if let Some(rest) = line.strip_prefix("edition") {
                let value = rest.trim_start().strip_prefix('=').map(|v| v.trim().trim_matches('"'));
                if let Some(year) = value.filter(|v| v.len() == 4 && v.bytes().all(|b| b.is_ascii_digit())) {
                    return year.to_string();
                }
            }
        }
        if !inherit {
            break;
        }
    }
    "2021".to_string()
}

const PRETTIER_CONFIGS: &[&str] = &[
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.json5",
    ".prettierrc.yml",
    ".prettierrc.yaml",
    ".prettierrc.toml",
    ".prettierrc.js",
    ".prettierrc.cjs",
    ".prettierrc.mjs",
    "prettier.config.js",
    "prettier.config.cjs",
    "prettier.config.mjs",
    "prettier.config.ts",
];

fn prettier(file: &Path, root: &Path) -> Option<Vec<String>> {
    let configured = find_up(file, root, PRETTIER_CONFIGS).is_some()
        || ancestors_within(file, root).any(|dir| file_contains(&dir.join("package.json"), "\"prettier\""));
    if !configured {
        return None;
    }
    let local = ancestors_within(file, root)
        .map(|dir| dir.join("node_modules/.bin/prettier"))
        .find(|bin| is_executable(bin));
    let program = local.or_else(|| which("prettier"))?;
    Some(vec![program.to_string_lossy().into_owned(), "--stdin-filepath".into(), "{file}".into()])
}

/// The formatter command for a file, or `None` when nothing should format it. `language` is
/// the language's display name ("Rust", "TypeScript"). In the command, `{file}` stands for
/// the file's path.
pub fn formatter_for(
    language: &str,
    file: &Path,
    root: &Path,
    configured: &HashMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    if let Some(command) = configured.get(language) {
        return if command.is_empty() { None } else { Some(command.clone()) };
    }
    let owned = |parts: &[&str]| -> Vec<String> { parts.iter().map(|p| p.to_string()).collect() };
    match language {
        "Rust" => {
            find_up(file, root, &["Cargo.toml"])?;
            which("rustfmt")?;
            let edition = rust_edition(file, root);
            Some(vec!["rustfmt".into(), "--edition".into(), edition, "--emit".into(), "stdout".into()])
        }
        "Go" => which("gofmt").map(|_| owned(&["gofmt"])),
        "Python" => {
            let pyproject = find_up(file, root, &["pyproject.toml"]);
            let has = |section: &str| pyproject.as_deref().is_some_and(|p| file_contains(p, section));
            if (find_up(file, root, &["ruff.toml", ".ruff.toml"]).is_some() || has("[tool.ruff")) && which("ruff").is_some() {
                Some(owned(&["ruff", "format", "--stdin-filename", "{file}", "-"]))
            } else if has("[tool.black") && which("black").is_some() {
                Some(owned(&["black", "-q", "--stdin-filename", "{file}", "-"]))
            } else {
                None
            }
        }
        "JavaScript" | "TypeScript" | "TSX" | "JSX" | "JSON" | "JSONC" | "CSS" | "SCSS" | "HTML" | "Markdown"
        | "YAML" | "Vue.js" | "Svelte" | "GraphQL" => prettier(file, root),
        "Terraform" | "HCL" if file.extension().is_some_and(|e| e == "tf" || e == "tfvars") => {
            which("terraform").map(|_| owned(&["terraform", "fmt", "-"]))
        }
        "Zig" => which("zig").map(|_| owned(&["zig", "fmt", "--stdin"])),
        "TOML" => {
            find_up(file, root, &["taplo.toml", ".taplo.toml"])?;
            which("taplo").map(|_| owned(&["taplo", "fmt", "-"]))
        }
        "Lua" => {
            find_up(file, root, &["stylua.toml", ".stylua.toml"])?;
            which("stylua").map(|_| owned(&["stylua", "-"]))
        }
        "C" | "C++" => {
            find_up(file, root, &[".clang-format", "_clang-format"])?;
            which("clang-format").map(|_| owned(&["clang-format", "--assume-filename={file}"]))
        }
        _ => None,
    }
}

/// Run a formatter over `input` and return what it printed. The command runs in `cwd` with
/// `{file}` in its arguments replaced by `file`. Blocks for at most `timeout`; call it from
/// a worker thread.
pub fn run(command: &[String], file: &Path, cwd: &Path, input: &str, timeout: Duration) -> Result<String, FormatError> {
    let Some((program, args)) = command.split_first() else {
        return Ok(input.to_string());
    };
    let file_text = file.to_string_lossy();
    let mut child = Command::new(program)
        .args(args.iter().map(|arg| arg.replace("{file}", &file_text)))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| FormatError::Spawn { program: program.clone(), source })?;

    // Feed stdin and drain both pipes on threads so a formatter that fills a pipe buffer
    // cannot deadlock against us.
    let mut stdin = child.stdin.take();
    let payload = input.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        if let Some(stdin) = stdin.as_mut() {
            let _ = stdin.write_all(&payload);
        }
        drop(stdin);
    });
    let mut stdout = child.stdout.take();
    let out_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(stdout) = stdout.as_mut() {
            let _ = stdout.read_to_end(&mut bytes);
        }
        bytes
    });
    let mut stderr = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(stderr) = stderr.as_mut() {
            let _ = stderr.read_to_end(&mut bytes);
        }
        bytes
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(FormatError::TimedOut { program: program.clone() });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(source) => return Err(FormatError::Spawn { program: program.clone(), source }),
        }
    };
    let _ = writer.join();
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();

    if !status.success() {
        let message = String::from_utf8_lossy(&stderr)
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("failed")
            .trim()
            .chars()
            .take(200)
            .collect();
        return Err(FormatError::Failed { program: program.clone(), message });
    }
    let output = String::from_utf8(stdout)
        .map_err(|_| FormatError::Failed { program: program.clone(), message: "output is not UTF-8".into() })?;
    // A formatter that exits 0 but prints nothing for real input is broken or misconfigured;
    // replacing the buffer with nothing would destroy the user's work.
    if output.trim().is_empty() && !input.trim().is_empty() {
        return Err(FormatError::EmptyOutput { program: program.clone() });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn runs_a_formatter_over_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&cmd(&["tr", "a-z", "A-Z"]), Path::new("/x.txt"), dir.path(), "hello\n", TIMEOUT).unwrap();
        assert_eq!(out, "HELLO\n");
    }

    #[test]
    fn file_placeholder_is_substituted() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(&cmd(&["sh", "-c", "cat >/dev/null; echo \"$0\"", "{file}"]), Path::new("/a b/x.rs"), dir.path(), "x", TIMEOUT)
            .unwrap();
        assert_eq!(out, "/a b/x.rs\n");
    }

    #[test]
    fn failures_carry_the_first_line_of_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let err = run(&cmd(&["sh", "-c", "echo; echo 'bad syntax at 3:4' >&2; echo more >&2; exit 2"]), Path::new("/x"), dir.path(), "x", TIMEOUT)
            .unwrap_err();
        assert_eq!(err.to_string(), "sh: bad syntax at 3:4");
        let err = run(&cmd(&["definitely-not-a-real-formatter"]), Path::new("/x"), dir.path(), "x", TIMEOUT).unwrap_err();
        assert!(matches!(err, FormatError::Spawn { .. }));
    }

    #[test]
    fn empty_output_never_replaces_real_text() {
        let dir = tempfile::tempdir().unwrap();
        let err = run(&cmd(&["sh", "-c", "cat >/dev/null"]), Path::new("/x"), dir.path(), "fn main() {}\n", TIMEOUT).unwrap_err();
        assert!(matches!(err, FormatError::EmptyOutput { .. }));
        // An empty file formatting to nothing is fine.
        assert_eq!(run(&cmd(&["cat"]), Path::new("/x"), dir.path(), "", TIMEOUT).unwrap(), "");
    }

    #[test]
    fn a_stuck_formatter_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let err = run(&cmd(&["sleep", "30"]), Path::new("/x"), dir.path(), "x", Duration::from_millis(150)).unwrap_err();
        assert!(matches!(err, FormatError::TimedOut { .. }));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn large_input_does_not_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let input = "line of text\n".repeat(200_000);
        let out = run(&cmd(&["cat"]), Path::new("/x"), dir.path(), &input, TIMEOUT).unwrap();
        assert_eq!(out.len(), input.len());
    }

    #[test]
    fn configured_formatters_win_and_can_disable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.rs");
        let mut configured = HashMap::new();
        configured.insert("Rust".to_string(), cmd(&["my-fmt", "{file}"]));
        assert_eq!(formatter_for("Rust", &file, dir.path(), &configured), Some(cmd(&["my-fmt", "{file}"])));
        configured.insert("Rust".to_string(), Vec::new());
        assert_eq!(formatter_for("Rust", &file, dir.path(), &configured), None);
    }

    #[test]
    fn nothing_formats_a_project_that_is_not_set_up_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let none = HashMap::new();
        for (language, name) in [("Rust", "a.rs"), ("Python", "a.py"), ("TypeScript", "a.ts"), ("TOML", "a.toml"), ("Plain Text", "a.txt")] {
            assert_eq!(formatter_for(language, &dir.path().join(name), dir.path(), &none), None, "{language}");
        }
    }

    #[test]
    fn rust_uses_the_manifest_edition() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("crates/a/src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace.package]\nedition = \"2024\"\n").unwrap();
        std::fs::write(root.join("crates/a/Cargo.toml"), "[package]\nname = \"a\"\nedition.workspace = true\n").unwrap();
        let file = root.join("crates/a/src/lib.rs");
        assert_eq!(rust_edition(&file, root), "2024", "inherited from the workspace");
        std::fs::write(root.join("crates/a/Cargo.toml"), "[package]\nedition = \"2018\"\n").unwrap();
        assert_eq!(rust_edition(&file, root), "2018");
        std::fs::write(root.join("crates/a/Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        assert_eq!(rust_edition(&file, root), "2021", "a manifest that says nothing means the default");
        if which("rustfmt").is_some() {
            let command = formatter_for("Rust", &file, root, &HashMap::new()).unwrap();
            assert_eq!(command[0], "rustfmt");
            let formatted = run(&command, &file, root, "fn  main( ) { }\n", TIMEOUT).unwrap();
            assert_eq!(formatted, "fn main() {}\n");
        }
    }

    #[test]
    fn prettier_needs_a_config_and_a_binary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src/a.ts");
        std::fs::write(root.join(".prettierrc"), "{}").unwrap();
        std::fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        let bin = root.join("node_modules/.bin/prettier");
        std::fs::write(&bin, "#!/bin/sh\ncat\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let command = formatter_for("TypeScript", &file, root, &HashMap::new()).unwrap();
        assert_eq!(command, vec![bin.to_string_lossy().into_owned(), "--stdin-filepath".to_string(), "{file}".to_string()]);
        // The config may also live in package.json.
        std::fs::remove_file(root.join(".prettierrc")).unwrap();
        std::fs::write(root.join("package.json"), "{\"prettier\": {}}").unwrap();
        assert!(formatter_for("Markdown", &root.join("README.md"), root, &HashMap::new()).is_some());
        std::fs::write(root.join("package.json"), "{}").unwrap();
        assert_eq!(formatter_for("TypeScript", &file, root, &HashMap::new()), None);
    }

    #[test]
    fn lookups_stop_at_the_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path();
        std::fs::write(outer.join(".prettierrc"), "{}").unwrap();
        std::fs::create_dir_all(outer.join("project/src")).unwrap();
        let root = outer.join("project");
        assert_eq!(find_up(&root.join("src/a.ts"), &root, PRETTIER_CONFIGS), None);
        assert!(find_up(&root.join("src/a.ts"), outer, PRETTIER_CONFIGS).is_some());
    }
}
