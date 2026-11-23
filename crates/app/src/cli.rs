//! Command line parsing.

use std::path::{Path, PathBuf};

pub const USAGE: &str = "\
bakedit - a lean terminal IDE

Usage: bakedit [options] [path[:line[:column]] ...]

With no path, opens the current directory. A directory becomes the project; files are
opened in it. When bakedit is already running, files are sent to that window (the one
whose project contains them, else the one used most recently) unless --new is given.

Paths may carry a position the way tools and agents print them:
  src/main.rs:42   src/main.rs:42:7   src/main.rs:42-60   src/main.rs#L42   file:///abs/main.rs#L42

Options:
  -n, --new                 start a new window even if one is running
      --window              open in a dedicated Ghostty window set up to pass every cmd key through
      --terminal <command>  run this in the terminal panel instead of the configured command
                            (\"shell\" for a plain shell)
      --no-restore          do not reopen the last session's tabs
      --dump-input          print every key and mouse event as bakedit decodes it (ctrl-c twice to quit)
      --probe               print what the host terminal supports and exit
      --print-keymap        print the built-in keymap and exit
      --print-ghostty-config  print the Ghostty config used by --window and exit
      --action <name> [json]  run an action in the running window and exit
      --log <file>          write a debug log
  -h, --help                show this help
  -V, --version             show the version
";

/// A file argument with an optional 1-based position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSpec {
    pub path: PathBuf,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// What to do instead of opening the editor.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Edit,
    DumpInput,
    Probe,
    PrintKeymap,
    PrintGhosttyConfig,
    Action { name: String, args: Option<serde_json::Value> },
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    pub mode: Mode,
    pub paths: Vec<PathSpec>,
    pub new_instance: bool,
    pub window: bool,
    pub terminal_command: Option<String>,
    pub no_restore: bool,
    pub log_file: Option<PathBuf>,
}

impl Default for Args {
    fn default() -> Args {
        Args {
            mode: Mode::Edit,
            paths: Vec::new(),
            new_instance: false,
            window: false,
            terminal_command: None,
            no_restore: false,
            log_file: None,
        }
    }
}

/// Parse the arguments after the program name. `cwd` anchors relative paths.
pub fn parse<I, S>(args: I, cwd: &Path) -> Result<Args, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut out = Args::default();
    let mut args = args.into_iter().map(Into::into).peekable();
    let mut only_paths = false;
    while let Some(arg) = args.next() {
        if only_paths || !arg.starts_with('-') || arg == "-" {
            out.paths.push(parse_path_spec(&arg, cwd));
            continue;
        }
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--" => only_paths = true,
            "-n" | "--new" => out.new_instance = true,
            "--window" => out.window = true,
            "--no-restore" => out.no_restore = true,
            "--terminal" => out.terminal_command = Some(value("--terminal")?),
            "--log" => out.log_file = Some(absolute(Path::new(&value("--log")?), cwd)),
            "--dump-input" => out.mode = Mode::DumpInput,
            "--probe" => out.mode = Mode::Probe,
            "--print-keymap" => out.mode = Mode::PrintKeymap,
            "--print-ghostty-config" => out.mode = Mode::PrintGhosttyConfig,
            "--action" => {
                let name = value("--action")?;
                let args_json = match args.peek() {
                    Some(next) if !next.starts_with("--") => args.next(),
                    _ => None,
                };
                let parsed = match args_json {
                    Some(text) => Some(
                        serde_json::from_str(&text).map_err(|e| format!("--action arguments are not valid JSON: {e}"))?,
                    ),
                    None => None,
                };
                out.mode = Mode::Action { name, args: parsed };
            }
            "-h" | "--help" => out.mode = Mode::Help,
            "-V" | "--version" => out.mode = Mode::Version,
            other => return Err(format!("unknown option {other} (try --help)")),
        }
    }
    Ok(out)
}

fn absolute(path: &Path, cwd: &Path) -> PathBuf {
    let expanded = expand_home(path);
    if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    }
}

fn expand_home(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

/// Split `path:line:col` style suffixes off a path argument and make the path absolute.
///
/// A literal path that exists is kept whole even if it looks like it has a suffix (a file
/// really named `notes:2`), the same rule the user's `zed-show` script uses.
pub fn parse_path_spec(raw: &str, cwd: &Path) -> PathSpec {
    let text = raw.strip_prefix("file://").map(percent_decode).unwrap_or_else(|| raw.to_string());
    let whole = absolute(Path::new(&text), cwd);
    if whole.exists() {
        return PathSpec { path: whole, line: None, column: None };
    }
    let (path, line, column) = split_position(&text);
    PathSpec { path: absolute(Path::new(path), cwd), line, column }
}

/// The position suffix forms: `#L10`, `#L10-L20`, `:10`, `:10:4`, `:10-20`, `(10,4)`.
fn split_position(text: &str) -> (&str, Option<u32>, Option<u32>) {
    let number = |s: &str| s.parse::<u32>().ok().filter(|n| *n > 0);

    if let Some((path, fragment)) = text.rsplit_once("#L") {
        let first = fragment.split('-').next().unwrap_or("");
        if let Some(line) = number(first) {
            if !path.is_empty() {
                return (path, Some(line), None);
            }
        }
    }
    if let Some(open) = text.rfind('(') {
        if let Some(inner) = text[open + 1..].strip_suffix(')') {
            let mut parts = inner.split([',', ':']);
            if let Some(line) = parts.next().and_then(number) {
                let column = parts.next().and_then(|c| number(c.trim()));
                if open > 0 && parts.next().is_none() {
                    return (&text[..open], Some(line), column);
                }
            }
        }
    }
    // `path:line:col`, `path:line-end`, `path:line`. A trailing colon (`path:12:`) is how
    // compilers print locations; it is ignored.
    let trimmed = text.strip_suffix(':').unwrap_or(text);
    let mut pieces: Vec<&str> = trimmed.rsplitn(3, ':').collect();
    pieces.reverse();
    match pieces.as_slice() {
        [path, line, column] if !path.is_empty() => {
            if let (Some(line), Some(column)) = (number(line), number(column)) {
                return (&trimmed[..path.len()], Some(line), Some(column));
            }
            // `a:b:12` where only the last piece is a number: the path contains a colon.
            let split = trimmed.len() - column.len() - 1;
            if let Some(line) = line_of(column) {
                return (&trimmed[..split], Some(line), None);
            }
        }
        [path, line] if !path.is_empty() => {
            if let Some(line) = line_of(line) {
                return (&trimmed[..path.len()], Some(line), None);
            }
        }
        _ => {}
    }
    (text, None, None)
}

/// `12` or the start of a range `12-30`.
fn line_of(text: &str) -> Option<u32> {
    let first = text.split('-').next()?;
    let rest_ok = text.split('-').skip(1).all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    if !rest_ok || text.split('-').count() > 2 {
        return None;
    }
    first.parse::<u32>().ok().filter(|n| *n > 0)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // `get` rather than indexing: the two bytes after a stray % may be half of a
            // multi-byte character.
            let hex = text.get(i + 1..i + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The project directory for a set of arguments: the first directory given, else the git
/// repository (or failing that the folder) of the first file, else the working directory.
pub fn project_root(paths: &[PathSpec], cwd: &Path) -> PathBuf {
    if let Some(dir) = paths.iter().find(|p| p.path.is_dir()) {
        return dir.path.clone();
    }
    if let Some(file) = paths.first() {
        let parent = file.path.parent().unwrap_or(cwd);
        return git_root(parent).unwrap_or_else(|| parent.to_path_buf());
    }
    cwd.to_path_buf()
}

/// The nearest ancestor (or the directory itself) that holds a `.git`.
pub fn git_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|dir| dir.join(".git").exists()).map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(raw: &str) -> (String, Option<u32>, Option<u32>) {
        let parsed = parse_path_spec(raw, Path::new("/work"));
        (parsed.path.to_string_lossy().into_owned(), parsed.line, parsed.column)
    }

    #[test]
    fn position_suffixes() {
        assert_eq!(spec("src/main.rs"), ("/work/src/main.rs".into(), None, None));
        assert_eq!(spec("src/main.rs:42"), ("/work/src/main.rs".into(), Some(42), None));
        assert_eq!(spec("src/main.rs:42:7"), ("/work/src/main.rs".into(), Some(42), Some(7)));
        assert_eq!(spec("src/main.rs:42-68"), ("/work/src/main.rs".into(), Some(42), None));
        assert_eq!(spec("src/main.rs#L42"), ("/work/src/main.rs".into(), Some(42), None));
        assert_eq!(spec("src/main.rs#L42-L68"), ("/work/src/main.rs".into(), Some(42), None));
        assert_eq!(spec("src/main.rs(42,7)"), ("/work/src/main.rs".into(), Some(42), Some(7)));
        assert_eq!(spec("src/main.rs(42)"), ("/work/src/main.rs".into(), Some(42), None));
        assert_eq!(spec("src/main.rs:42:"), ("/work/src/main.rs".into(), Some(42), None));
        assert_eq!(spec("/abs/x.py:3"), ("/abs/x.py".into(), Some(3), None));
    }

    #[test]
    fn file_urls() {
        assert_eq!(spec("file:///abs/my%20file.rs#L10"), ("/abs/my file.rs".into(), Some(10), None));
        assert_eq!(spec("file:///abs/a.rs"), ("/abs/a.rs".into(), None, None));
        assert_eq!(spec("file:///abs/100%.txt"), ("/abs/100%.txt".into(), None, None));
    }

    #[test]
    fn things_that_are_not_positions() {
        assert_eq!(spec("notes"), ("/work/notes".into(), None, None));
        assert_eq!(spec("a:b"), ("/work/a:b".into(), None, None));
        assert_eq!(spec("v1.2:0"), ("/work/v1.2:0".into(), None, None), "line 0 is not a line");
        assert_eq!(spec("a:b:12"), ("/work/a:b".into(), Some(12), None));
        assert_eq!(spec("x:1-2-3"), ("/work/x:1-2-3".into(), None, None));
        assert_eq!(spec(":12"), ("/work/:12".into(), None, None));
    }

    #[test]
    fn an_existing_file_with_a_colon_name_is_kept_whole() {
        let dir = tempfile::tempdir().unwrap();
        let odd = dir.path().join("notes:2");
        std::fs::write(&odd, "x").unwrap();
        let parsed = parse_path_spec("notes:2", dir.path());
        assert_eq!(parsed, PathSpec { path: odd, line: None, column: None });
        // When it does not exist the suffix is a position.
        let parsed = parse_path_spec("other:2", dir.path());
        assert_eq!(parsed.line, Some(2));
    }

    #[test]
    fn home_is_expanded() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(parse_path_spec("~/x.rs:3", Path::new("/work")).path, home.join("x.rs"));
    }

    #[test]
    fn options() {
        let cwd = Path::new("/work");
        let args = parse(["-n", "--no-restore", "--terminal", "shell", "a.rs:3", "--log", "debug.log"], cwd).unwrap();
        assert!(args.new_instance && args.no_restore);
        assert_eq!(args.terminal_command.as_deref(), Some("shell"));
        assert_eq!(args.paths.len(), 1);
        assert_eq!(args.paths[0].line, Some(3));
        assert_eq!(args.log_file, Some(PathBuf::from("/work/debug.log")));
        assert_eq!(args.mode, Mode::Edit);

        assert_eq!(parse(["--help"], cwd).unwrap().mode, Mode::Help);
        assert_eq!(parse(["-V"], cwd).unwrap().mode, Mode::Version);
        assert_eq!(parse(["--dump-input"], cwd).unwrap().mode, Mode::DumpInput);
        assert!(parse(["--window"], cwd).unwrap().window);
        assert!(parse(["--nope"], cwd).is_err());
        assert!(parse(["--terminal"], cwd).is_err());
    }

    #[test]
    fn double_dash_ends_options() {
        let args = parse(["--", "--weird-name", "-n"], Path::new("/work")).unwrap();
        assert!(!args.new_instance);
        assert_eq!(args.paths.len(), 2);
        assert_eq!(args.paths[0].path, PathBuf::from("/work/--weird-name"));
    }

    #[test]
    fn action_mode() {
        let cwd = Path::new("/work");
        let args = parse(["--action", "workspace::Save"], cwd).unwrap();
        assert_eq!(args.mode, Mode::Action { name: "workspace::Save".into(), args: None });
        let args = parse(["--action", "editor::GoToLine", "{\"line\": 4}"], cwd).unwrap();
        assert_eq!(
            args.mode,
            Mode::Action { name: "editor::GoToLine".into(), args: Some(serde_json::json!({"line": 4})) }
        );
        assert!(parse(["--action", "x", "{not json"], cwd).is_err());
        let args = parse(["--action", "zed::Quit", "--new"], cwd).unwrap();
        assert!(args.new_instance, "a following option is not taken as the arguments");
    }

    #[test]
    fn project_root_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("repo/.git")).unwrap();
        std::fs::create_dir_all(root.join("repo/src/deep")).unwrap();
        std::fs::create_dir_all(root.join("plain/sub")).unwrap();
        std::fs::write(root.join("repo/src/deep/a.rs"), "").unwrap();
        std::fs::write(root.join("plain/sub/b.txt"), "").unwrap();
        let spec = |p: &str| parse_path_spec(p, &root);

        assert_eq!(project_root(&[], &root), root);
        assert_eq!(project_root(&[spec("repo/src/deep/a.rs:3")], &root), root.join("repo"));
        assert_eq!(project_root(&[spec("plain/sub/b.txt")], &root), root.join("plain/sub"));
        assert_eq!(
            project_root(&[spec("plain/sub/b.txt"), spec("repo/src")], &root),
            root.join("repo/src"),
            "a directory argument wins"
        );
        assert_eq!(git_root(&root.join("repo/src/deep")), Some(root.join("repo")));
        assert_eq!(git_root(&root.join("plain")), git_root(&root));
    }
}
