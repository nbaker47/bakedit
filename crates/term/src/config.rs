//! What a terminal runs, how big it starts and which colours it reports.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bakedit_core::{Color, Rgb, Theme};

use crate::error::{Result, TermError};

/// The command a terminal panel runs when the user has not chosen another: herdr.
pub const DEFAULT_COMMAND: &str = "herdr";

/// Everything [`Terminal::spawn`](crate::Terminal::spawn) needs to know.
///
/// Three constructors cover what a terminal panel is asked to run: [`TermConfig::herdr`]
/// (the default), [`TermConfig::shell`] and [`TermConfig::command`] for a command line the
/// user wrote in their settings. The rest are plain fields; set them with struct update
/// syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermConfig {
    /// The program to run, looked up on `PATH` unless it has a slash in it. `None` runs
    /// the user's login shell (`$SHELL`, else the account's shell, else `/bin/sh`) as a
    /// login shell.
    pub program: Option<String>,
    /// Arguments for the program (or for the login shell).
    pub args: Vec<String>,
    /// Where the program starts. `None`, or a directory that does not exist, means the
    /// directory this process is in.
    pub cwd: Option<PathBuf>,
    /// Variables to set for the child, on top of this process's environment and the ones
    /// every terminal sets (`TERM`, `COLORTERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`).
    pub env: Vec<(String, String)>,
    /// Variables the child must not inherit. Applied last, so it wins over `env`.
    pub env_remove: Vec<String>,
    /// Width in cells. At least 2.
    pub cols: u16,
    /// Height in cells. At least 1.
    pub rows: u16,
    /// Width of one cell in pixels, for programs that ask (image protocols, mostly).
    pub cell_width: u16,
    /// Height of one cell in pixels.
    pub cell_height: u16,
    /// Lines of scrollback kept for the main screen.
    pub scrollback: usize,
    /// The colours reported to a child that asks (OSC 4, 10, 11 and 12).
    pub colors: TermColors,
    /// The shortest time between two [`TermEvent::Wakeup`](crate::TermEvent::Wakeup)s.
    pub wakeup_interval: Duration,
    /// How many arrow keys one wheel step sends to a full-screen program that has not
    /// asked for the mouse (alternate scroll mode).
    pub alt_scroll_lines: u16,
}

impl Default for TermConfig {
    /// The user's login shell in an 80x24 terminal.
    fn default() -> TermConfig {
        TermConfig {
            program: None,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
            scrollback: 10_000,
            colors: TermColors::default(),
            wakeup_interval: Duration::from_millis(8),
            alt_scroll_lines: 3,
        }
    }
}

impl TermConfig {
    /// The user's login shell. The same as [`TermConfig::default`].
    pub fn shell() -> TermConfig {
        TermConfig::default()
    }

    /// herdr: [`DEFAULT_COMMAND`], found on `PATH`, attaching to the user's default session.
    ///
    /// Spawning this fails with an error for which [`TermError::is_program_missing`] is
    /// true when herdr is not installed. Inside a herdr pane it starts, says that nested
    /// herdr is disabled and exits with status 1; see [`TermConfig::default_command`].
    pub fn herdr() -> TermConfig {
        TermConfig {
            program: Some(DEFAULT_COMMAND.to_string()),
            ..TermConfig::default()
        }
    }

    /// What a terminal panel runs when the user has not chosen a command: herdr, or the
    /// login shell when this process is itself running inside a herdr pane
    /// ([`inside_herdr`]).
    ///
    /// herdr does not run inside itself, and it must not be talked into it by hiding where
    /// it is: started from one of its own panes with the `HERDR_*` variables removed, it
    /// would attach to the very session it is being displayed in.
    pub fn default_command() -> TermConfig {
        default_command_when(inside_herdr())
    }

    /// A command line as the user writes it in their settings: `herdr`,
    /// `herdr --session work`, `tmux new -A -s main`, `~/bin/dev shell`.
    ///
    /// The line is split by [`split_command_line`]; no shell is involved, so there are no
    /// variables, globs or pipes (write `sh -c '...'` for those). An empty line is an
    /// error; use [`TermConfig::shell`] for "no command".
    pub fn command(line: &str) -> Result<TermConfig> {
        let mut words = split_command_line(line)?.into_iter();
        let program = words
            .next()
            .ok_or_else(|| TermError::CommandLine("the command is empty".to_string()))?;
        Ok(TermConfig {
            program: Some(program),
            args: words.collect(),
            ..TermConfig::default()
        })
    }
}

fn default_command_when(inside_herdr: bool) -> TermConfig {
    if inside_herdr {
        TermConfig::shell()
    } else {
        TermConfig::herdr()
    }
}

/// Whether this process is running inside a herdr pane. herdr sets `HERDR_ENV=1` for
/// everything it starts, and refuses to start where it finds it.
pub fn inside_herdr() -> bool {
    is_herdr_marker(std::env::var("HERDR_ENV").ok().as_deref())
}

fn is_herdr_marker(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Split a command line into words the way a POSIX shell would, without running one.
///
/// Words are separated by spaces and tabs. Single quotes keep everything literal; double
/// quotes keep everything but `\"`, `\\`, `` \` `` and `\$`; a backslash outside quotes
/// makes the next character literal. A word that starts with an unquoted `~/` (or is just
/// `~`) has the `~` replaced by `$HOME`. Nothing else is expanded.
///
/// An empty or all-blank line is `Ok` with no words. A quote that is never closed and a
/// backslash at the very end are errors.
pub fn split_command_line(line: &str) -> Result<Vec<String>> {
    let home = std::env::var("HOME").ok().filter(|home| !home.is_empty());
    split_words(line, home.as_deref())
}

fn split_words(line: &str, home: Option<&str>) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    // A word exists once anything, even an empty pair of quotes, has been seen for it.
    let mut in_word = false;
    // Whether the word began with a `~` that no quote or backslash protected.
    let mut tilde = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if in_word {
                    words.push(finish_word(std::mem::take(&mut word), tilde, home));
                    in_word = false;
                    tilde = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => {
                            return Err(TermError::CommandLine(
                                "a single quote is never closed".to_string(),
                            ))
                        }
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '`' | '$')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => {
                                return Err(TermError::CommandLine(
                                    "a double quote is never closed".to_string(),
                                ))
                            }
                        },
                        Some(c) => word.push(c),
                        None => {
                            return Err(TermError::CommandLine(
                                "a double quote is never closed".to_string(),
                            ))
                        }
                    }
                }
            }
            '\\' => match chars.next() {
                Some(c) => {
                    in_word = true;
                    word.push(c);
                }
                None => {
                    return Err(TermError::CommandLine(
                        "a backslash at the end escapes nothing".to_string(),
                    ))
                }
            },
            c => {
                if !in_word && c == '~' {
                    tilde = true;
                }
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(finish_word(word, tilde, home));
    }
    Ok(words)
}

fn finish_word(word: String, tilde: bool, home: Option<&str>) -> String {
    match (tilde, home) {
        (true, Some(home)) if word == "~" => home.to_string(),
        (true, Some(home)) if word.starts_with("~/") => format!("{home}{}", &word[1..]),
        _ => word,
    }
}

/// The names of the variables in this process's environment that start with `prefix`,
/// sorted, for [`TermConfig::env_remove`].
///
/// `inherited_env_names("HERDR_")` makes a program forget that it was started inside
/// herdr. Only do that for a herdr given a session of its own (`herdr --session name`);
/// see [`TermConfig::default_command`] for why.
pub fn inherited_env_names(prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| name.starts_with(prefix))
        .collect();
    names.sort();
    names
}

/// Find the executable a program name refers to, the way running it would: a name with a
/// slash is that path, anything else is searched for along `PATH`. `None` when there is no
/// such executable file.
pub fn find_program(program: &str) -> Option<PathBuf> {
    find_program_in(program, std::env::var_os("PATH").as_deref())
}

fn find_program_in(program: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        let candidate = PathBuf::from(program);
        return is_executable(&candidate).then_some(candidate);
    }
    std::env::split_paths(path?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// The colours a terminal reports when the child asks what they are.
///
/// Cells are handed to the application with [`Color::Default`] and [`Color::Indexed`]
/// intact, so the application's theme decides how they look; these values are what
/// programs see when they query the terminal (to tell a dark background from a light one,
/// say). Keep them in step with the theme: [`TermColors::from_theme`] at start and
/// [`Terminal::set_colors`](crate::Terminal::set_colors) when the theme changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermColors {
    /// Default text colour (OSC 10).
    pub fg: Rgb,
    /// Default background (OSC 11).
    pub bg: Rgb,
    /// Cursor colour (OSC 12).
    pub cursor: Rgb,
    /// The 16 ANSI colours: 0-7 normal, 8-15 bright.
    pub ansi: [Rgb; 16],
}

impl Default for TermColors {
    /// xterm's colours on a black background.
    fn default() -> TermColors {
        let c = Rgb::new;
        TermColors {
            fg: c(0xe5, 0xe5, 0xe5),
            bg: c(0x00, 0x00, 0x00),
            cursor: c(0xe5, 0xe5, 0xe5),
            ansi: [
                c(0x00, 0x00, 0x00),
                c(0xcd, 0x00, 0x00),
                c(0x00, 0xcd, 0x00),
                c(0xcd, 0xcd, 0x00),
                c(0x00, 0x00, 0xee),
                c(0xcd, 0x00, 0xcd),
                c(0x00, 0xcd, 0xcd),
                c(0xe5, 0xe5, 0xe5),
                c(0x7f, 0x7f, 0x7f),
                c(0xff, 0x00, 0x00),
                c(0x00, 0xff, 0x00),
                c(0xff, 0xff, 0x00),
                c(0x5c, 0x5c, 0xff),
                c(0xff, 0x00, 0xff),
                c(0x00, 0xff, 0xff),
                c(0xff, 0xff, 0xff),
            ],
        }
    }
}

impl TermColors {
    /// The terminal colours of a theme.
    pub fn from_theme(theme: &Theme) -> TermColors {
        TermColors {
            fg: theme.terminal_fg,
            bg: theme.terminal_bg,
            cursor: theme.cursor,
            ansi: theme.ansi,
        }
    }

    /// The colour of palette entry `index`: the 16 ANSI colours, then xterm's 6x6x6 colour
    /// cube (16-231) and its grey ramp (232-255).
    pub fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[usize::from(index)],
            16..=231 => {
                let index = index - 16;
                let level = |step: u8| if step == 0 { 0 } else { 55 + 40 * step };
                Rgb::new(level(index / 36), level((index / 6) % 6), level(index % 6))
            }
            232..=255 => {
                let grey = 8 + 10 * (index - 232);
                Rgb::new(grey, grey, grey)
            }
        }
    }

    /// Turn a cell colour into RGB: palette entries through [`TermColors::indexed`], and
    /// `default` for [`Color::Default`] (pass `self.fg` or `self.bg`, whichever the colour
    /// is for).
    pub fn resolve(&self, color: Color, default: Rgb) -> Rgb {
        match color {
            Color::Default => default,
            Color::Indexed(index) => self.indexed(index),
            Color::Rgb(rgb) => rgb,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        split_words(line, Some("/home/me")).unwrap()
    }

    fn error(line: &str) -> String {
        split_words(line, Some("/home/me")).unwrap_err().to_string()
    }

    #[test]
    fn splits_on_blanks() {
        assert_eq!(words("herdr"), ["herdr"]);
        assert_eq!(
            words("herdr --session work"),
            ["herdr", "--session", "work"]
        );
        assert_eq!(
            words("  tmux\tnew  -A   -s main  "),
            ["tmux", "new", "-A", "-s", "main"]
        );
        assert_eq!(words(""), Vec::<String>::new());
        assert_eq!(words("   \t "), Vec::<String>::new());
    }

    #[test]
    fn quotes_group_and_can_be_empty() {
        assert_eq!(
            words("sh -c 'echo hi; exec zsh'"),
            ["sh", "-c", "echo hi; exec zsh"]
        );
        assert_eq!(
            words("sh -c \"echo \\\"hi\\\" $HOME\""),
            ["sh", "-c", "echo \"hi\" $HOME"]
        );
        assert_eq!(words("a'b c'd\"e f\"g"), ["ab cde fg"]);
        assert_eq!(words("run '' \"\" x"), ["run", "", "", "x"]);
        assert_eq!(words("'it''s'"), ["its"]);
        assert_eq!(words("\"a\\nb\" 'a\\nb'"), ["a\\nb", "a\\nb"]);
        assert_eq!(
            words("\"a\\\\b\" \"a\\$b\" \"a\\`b\""),
            ["a\\b", "a$b", "a`b"]
        );
    }

    #[test]
    fn backslash_makes_the_next_character_literal() {
        assert_eq!(words("my\\ program --flag"), ["my program", "--flag"]);
        assert_eq!(words("a\\'b \\\"c\\\" d\\\\e"), ["a'b", "\"c\"", "d\\e"]);
        assert_eq!(words("\\~/x"), ["~/x"]);
    }

    #[test]
    fn unfinished_quoting_is_an_error() {
        assert!(error("sh -c 'oops").contains("single quote"));
        assert!(error("sh -c \"oops").contains("double quote"));
        assert!(error("sh -c \"oops\\").contains("double quote"));
        assert!(error("oops\\").contains("backslash"));
    }

    #[test]
    fn a_leading_tilde_is_the_home_directory() {
        assert_eq!(words("~/bin/dev shell"), ["/home/me/bin/dev", "shell"]);
        assert_eq!(words("ls ~ ~/x"), ["ls", "/home/me", "/home/me/x"]);
        // Only at the start of a word, only when unquoted, and never `~user`.
        assert_eq!(words("a~/b x=~/y"), ["a~/b", "x=~/y"]);
        assert_eq!(words("'~/quoted' \"~/quoted\""), ["~/quoted", "~/quoted"]);
        assert_eq!(words("~other/bin ~~"), ["~other/bin", "~~"]);
        // Without a home there is nothing to expand to.
        assert_eq!(split_words("~/bin/dev", None).unwrap(), ["~/bin/dev"]);
    }

    #[test]
    fn command_builds_a_config() {
        let config = TermConfig::command("tmux new -A -s 'my session'").unwrap();
        assert_eq!(config.program.as_deref(), Some("tmux"));
        assert_eq!(config.args, ["new", "-A", "-s", "my session"]);
        assert!(config.env_remove.is_empty());
        assert_eq!((config.cols, config.rows), (80, 24));

        assert!(matches!(
            TermConfig::command(""),
            Err(TermError::CommandLine(_))
        ));
        assert!(matches!(
            TermConfig::command("   "),
            Err(TermError::CommandLine(_))
        ));
        assert!(matches!(
            TermConfig::command("sh -c 'x"),
            Err(TermError::CommandLine(_))
        ));
    }

    #[test]
    fn herdr_is_the_default_command_unless_already_inside_it() {
        let herdr = TermConfig::herdr();
        assert_eq!(herdr.program.as_deref(), Some(DEFAULT_COMMAND));
        assert_eq!(DEFAULT_COMMAND, "herdr");
        assert!(herdr.args.is_empty());
        // Nothing is hidden from it: its own refusal to nest has to keep working.
        assert!(herdr.env_remove.is_empty());
        assert!(TermConfig::command("herdr --session work")
            .unwrap()
            .env_remove
            .is_empty());

        assert_eq!(default_command_when(false), TermConfig::herdr());
        assert_eq!(default_command_when(true), TermConfig::shell());
        assert_eq!(
            TermConfig::default_command(),
            default_command_when(inside_herdr())
        );

        assert!(is_herdr_marker(Some("1")));
        assert!(!is_herdr_marker(None));
        assert!(!is_herdr_marker(Some("")));
        assert!(!is_herdr_marker(Some("0")));
    }

    #[test]
    fn the_default_is_a_login_shell() {
        let config = TermConfig::default();
        assert_eq!(config.program, None);
        assert_eq!(config, TermConfig::shell());
        assert_eq!(
            (config.cell_width, config.cell_height, config.scrollback),
            (8, 16, 10_000)
        );
    }

    #[test]
    fn inherited_names_match_the_prefix_and_are_sorted() {
        let names = inherited_env_names("");
        assert!(names.windows(2).all(|pair| pair[0] <= pair[1]));
        if std::env::var_os("PATH").is_some() {
            assert!(inherited_env_names("PA").contains(&"PATH".to_string()));
        }
        assert!(inherited_env_names("BAKEDIT_NO_SUCH_PREFIX_").is_empty());
    }

    #[test]
    fn programs_are_found_like_the_system_finds_them() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let data = bin.join("data");
        std::fs::write(&data, "not a program").unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o644)).unwrap();

        let path = std::env::join_paths([dir.path().join("empty"), bin.clone()]).unwrap();
        assert_eq!(find_program_in("tool", Some(&path)), Some(tool.clone()));
        assert_eq!(find_program_in("data", Some(&path)), None);
        assert_eq!(find_program_in("missing", Some(&path)), None);
        assert_eq!(find_program_in("bin", Some(&path)), None);
        assert_eq!(find_program_in("tool", None), None);
        assert_eq!(find_program_in("", Some(&path)), None);
        // A name with a slash is a path, not something to search for.
        assert_eq!(find_program_in(tool.to_str().unwrap(), None), Some(tool));
        assert_eq!(find_program_in(data.to_str().unwrap(), Some(&path)), None);
        assert_eq!(find_program_in("bin/tool", Some(&path)), None);
        assert!(find_program("sh").is_some());
    }

    #[test]
    fn palette_follows_xterm() {
        let colors = TermColors::default();
        assert_eq!(colors.indexed(1), colors.ansi[1]);
        assert_eq!(colors.indexed(15), Rgb::new(255, 255, 255));
        assert_eq!(colors.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(colors.indexed(21), Rgb::new(0, 0, 255));
        assert_eq!(colors.indexed(46), Rgb::new(0, 255, 0));
        assert_eq!(colors.indexed(196), Rgb::new(255, 0, 0));
        assert_eq!(colors.indexed(110), Rgb::new(0x87, 0xaf, 0xd7));
        assert_eq!(colors.indexed(231), Rgb::new(255, 255, 255));
        assert_eq!(colors.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(colors.indexed(244), Rgb::new(128, 128, 128));
        assert_eq!(colors.indexed(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn colours_resolve_against_the_right_default() {
        let colors = TermColors::default();
        assert_eq!(colors.resolve(Color::Default, colors.fg), colors.fg);
        assert_eq!(colors.resolve(Color::Default, colors.bg), colors.bg);
        assert_eq!(colors.resolve(Color::Indexed(4), colors.fg), colors.ansi[4]);
        assert_eq!(
            colors.resolve(Color::Rgb(Rgb::new(1, 2, 3)), colors.fg),
            Rgb::new(1, 2, 3)
        );
    }

    #[test]
    fn theme_colours_carry_over() {
        let theme = Theme::builtin();
        let colors = TermColors::from_theme(&theme);
        assert_eq!(colors.fg, theme.terminal_fg);
        assert_eq!(colors.bg, theme.terminal_bg);
        assert_eq!(colors.cursor, theme.cursor);
        assert_eq!(colors.ansi, theme.ansi);
    }
}
