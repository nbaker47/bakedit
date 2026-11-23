//! bakedit in a Ghostty window of its own, set up so that every cmd chord reaches bakedit
//! instead of Ghostty (new tab, close, quit, splits, search...). A plain `bakedit` opens one
//! (on macOS, unless the `"window"` setting is false); [`decide`] says where a command runs:
//! a new window, the window already running, or the current terminal (`--here`).
//!
//! The settings come from the research in `.reference/phase1/host-terminal-research.md`
//! (Ghostty 1.4): drop every Ghostty binding (`keybind = clear`) so unbound chords go to the
//! program through the kitty keyboard protocol bakedit turns on; keep only font zoom and
//! paste; Option is Alt on the left; no link handling of Ghostty's own, since bakedit opens
//! cmd+clicked paths itself. Two files are written under `~/.config/bakedit/ghostty/` (never
//! the user's Ghostty config): `bakedit.ghostty` includes the user's own Ghostty config for
//! the font and colours, then `bakedit-window.ghostty`, which has the last word.
//!
//! The keymap helps in one place: chords the macOS menu bar takes before any binding
//! (cmd-h, cmd-alt-h, cmd-m) are handed to bakedit with a fixed escape sequence only when
//! the keymap binds them, and left to macOS otherwise, as in Zed.
//!
//! The window is opened with `open -na Ghostty.app --args ... -e bakedit --here --new
//! --cwd=<dir> ...` from the current environment minus what would mislead the new process:
//! herdr's `HERDR_*` variables (it is no longer inside a herdr pane, so its terminal panel
//! should run herdr), and those of terminal multiplexers. `--here` and the `BAKEDIT_HOST`
//! the config sets both stop the bakedit in the window from opening another one.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use bakedit_keymap::{Keymap, Keystroke};

use crate::cli::{Args, Mode};

/// Bumped when the generated files change in a way that matters; written into the window
/// so bakedit can tell an out-of-date window from a current one (`BAKEDIT_HOST_CONFIG`).
pub const CONFIG_VERSION: u32 = 1;

/// The environment value the window gives bakedit, so it knows every cmd chord is coming.
pub const HOST_NAME: &str = "ghostty-dedicated";

/// The variables the window's config sets for bakedit. Programs in bakedit's terminal
/// panel must not inherit them: they are not the bakedit the window runs.
pub const HOST_VARIABLES: &[&str] = &["BAKEDIT_HOST", "BAKEDIT_HOST_CONFIG"];

/// Chords AppKit's menu bar takes before a terminal sees them, and the bytes a Ghostty
/// binding sends instead (the kitty protocol's spelling of the same press).
const MENU_CHORDS: &[(&str, &str, &str)] = &[
    ("cmd-h", "super+h", "csi:104;9u"),
    ("cmd-alt-h", "super+alt+h", "csi:104;11u"),
    ("cmd-m", "super+m", "csi:109;9u"),
];

/// Variables never passed to the window's process: herdr's (the new window is not inside a
/// herdr pane) and terminal multiplexers' (it is not inside those either).
const SCRUBBED_PREFIXES: &[&str] = &["HERDR_", "TMUX", "ZELLIJ", "CMUX_", "GHOSTTY_SURFACE_ID"];

/// The two Ghostty config files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhosttyConfig {
    /// `bakedit.ghostty`: the entry file named on Ghostty's command line.
    pub entry: String,
    /// `bakedit-window.ghostty`: the window's own settings.
    pub window: String,
}

/// Where the files are written.
pub fn config_dir() -> PathBuf {
    crate::settings::config_dir().join("ghostty")
}

/// Whether the keymap binds anything that starts with `chord`.
fn binds(keymap: &Keymap, chord: &str) -> bool {
    let Ok(wanted) = Keystroke::parse(chord) else { return false };
    keymap.bindings().iter().any(|binding| !binding.is_null() && binding.keystrokes().first() == Some(&wanted))
}

/// The config files for a window running bakedit with `keymap`.
pub fn ghostty_config(keymap: &Keymap) -> GhosttyConfig {
    let mut menu = String::new();
    for (chord, ghostty, bytes) in MENU_CHORDS {
        if binds(keymap, chord) {
            menu.push_str(&format!("keybind = {ghostty}={bytes}\n"));
        } else {
            menu.push_str(&format!("# keybind = {ghostty}={bytes}\n"));
        }
    }
    let window = format!(
        r#"# bakedit-window.ghostty
# Ghostty 1.4 settings for a window that runs only bakedit. Written by bakedit {version}
# each time it opens a window; edit ~/.config/bakedit/keymap.json, not this file.
#
# Loaded after your own Ghostty config (see bakedit.ghostty), so that it wins.

# --- Identity ---------------------------------------------------------------
# bakedit reads BAKEDIT_HOST to know every Command chord is being forwarded; the
# number lets it notice an out-of-date copy of this file.
env = BAKEDIT_HOST={host}
env = BAKEDIT_HOST_CONFIG={config_version}

# --- Keyboard: forward everything -------------------------------------------
# Drop every default binding (new tab, close, quit, splits, search, font size,
# Cmd+arrow -> Ctrl+A/E, Option+arrow -> ESC b/f, Shift+arrow selection ...). An
# unbound chord is not a menu shortcut either, so the key goes to bakedit,
# encoded with the kitty keyboard protocol (Command = modifier bit 8). This
# includes Cmd+Q, Cmd+W, Cmd+N and Cmd+T.
keybind = clear

# Zoom stays with the terminal: the font size is the only way to scale a
# terminal UI, and it matches Zed's Cmd+= / Cmd+- / Cmd+0.
keybind = super+==increase_font_size:1
keybind = super++=increase_font_size:1
keybind = super+-=decrease_font_size:1
keybind = super+0=reset_font_size

# Paste stays with the terminal: Ghostty reads the macOS clipboard and sends one
# bracketed paste. With no text on the clipboard Cmd+V passes through as a key.
keybind = performable:super+v=paste_from_clipboard

# Chords the menu bar takes first: Hide (Cmd+H), Hide Others (Opt+Cmd+H),
# Minimize (Cmd+M). Each one your keymap uses is handed to bakedit as the
# bytes shown (with no key release); the others are left to macOS, as in Zed.
{menu}
# --- Option key ---------------------------------------------------------------
# Left Option is Alt: Alt+arrows, Alt+Backspace and Alt+letters arrive as Alt,
# without dead keys. Right Option still types characters such as £ and €.
macos-option-as-alt = left

# --- Mouse and links ------------------------------------------------------------
# Shift+click and Shift+drag go to bakedit (extend a selection).
mouse-shift-capture = true

# bakedit opens Cmd+clicked paths and URLs itself, in its editor.
link-url = false
link-osc8 = false
link-previews = false
cursor-click-to-move = false

# --- Clipboard ----------------------------------------------------------------
clipboard-read = allow
clipboard-write = allow
clipboard-paste-protection = false
copy-on-select = none

# --- Window lifecycle -----------------------------------------------------------
# Quit this Ghostty instance when bakedit exits and its window closes.
quit-after-last-window-closed = true
# Ask before the red close button or the Quit menu item kills a running bakedit.
confirm-close-surface = true
# Kept on purpose, from Ghostty 1.4.0's source (macos/Sources/...):
# - `never` makes this window not restorable (Features/Terminal/
#   TerminalController.swift:586) and saves no app state (App/AppDelegate.swift:920),
#   so your own Ghostty, which shares this instance's saved-state folder, can never
#   bring back a bakedit window.
# - `never` makes this instance restore nothing when it starts (Features/Terminal/
#   TerminalRestorable.swift:142), so it cannot take over your saved windows, and the
#   bakedit window always opens (App/AppDelegate.swift:376 opens the first window only
#   when nothing was restored).
# - Without it, this window would be restorable: the first window has no command of
#   its own (Features/Terminal/TerminalController.swift:74).
# The cost: Ghostty writes every value of this setting into the preferences it
# shares with your Ghostty (App/AppDelegate.swift:818-822; UserDefaults.ghostty is
# com.mitchellh.ghostty in release builds, Helpers/Extensions/UserDefaults+Extension.swift:12).
# `never` sets NSQuitAlwaysKeepsWindows = false there until your Ghostty next loads
# its config (at launch or on a reload, App/AppDelegate.swift:230) and writes its own
# value. With window-save-state = default and macOS's "Close windows when quitting
# an application" on, that changes nothing; with `always`, or that setting off,
# quitting your Ghostty meanwhile does not keep its windows. No value avoids the write.
window-save-state = never
# The command is not a shell: no shell integration.
shell-integration = none

# --- Appearance -----------------------------------------------------------------
# Font, theme and colours come from your own Ghostty config. The UI draws to the
# edges: no padding, the edge cells' background extended into the margin.
window-padding-x = 0
window-padding-y = 0
window-padding-balance = true
window-padding-color = extend
resize-overlay = never
"#,
        version = env!("CARGO_PKG_VERSION"),
        host = HOST_NAME,
        config_version = CONFIG_VERSION,
        menu = menu,
    );
    let entry = r#"# bakedit.ghostty
# The entry file of a bakedit window, used as
#   --config-default-files=false --config-file=<this file>
# Ghostty loads the files named here after this one, in order: your own Ghostty
# config supplies the font and colours, and bakedit-window.ghostty has the last
# word. A leading `?` makes a missing file silent.
config-file = ?~/.config/ghostty/config
config-file = ?~/.config/ghostty/config.ghostty
config-file = ?~/Library/Application Support/com.mitchellh.ghostty/config
config-file = ?~/Library/Application Support/com.mitchellh.ghostty/config.ghostty
config-file = bakedit-window.ghostty
config-file = ?bakedit-user.ghostty
"#
    .to_string();
    GhosttyConfig { entry, window }
}

/// Write the files into `dir` (only when their text changed) and return the entry file.
pub fn write_config(dir: &Path, config: &GhosttyConfig) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for (name, text) in [("bakedit.ghostty", &config.entry), ("bakedit-window.ghostty", &config.window)] {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            std::fs::write(&path, text)?;
        }
    }
    Ok(dir.join("bakedit.ghostty"))
}

/// Where Ghostty is installed, if it is.
pub fn find_ghostty() -> Option<PathBuf> {
    let mut places = vec![PathBuf::from("/Applications/Ghostty.app")];
    if let Some(home) = dirs::home_dir() {
        places.push(home.join("Applications/Ghostty.app"));
    }
    places.into_iter().find(|path| path.is_dir())
}

/// Whether this process is the bakedit a dedicated window runs: the window's config sets
/// `BAKEDIT_HOST` for it. Such a bakedit never opens another window.
pub fn running_in_window() -> bool {
    is_set(std::env::var_os("BAKEDIT_HOST").as_deref())
}

/// Whether this process runs in ptyshot, the pty the scenario tests drive bakedit in
/// (`TERM_PROGRAM=ptyshot`). A bakedit under test never opens a Ghostty window, even when
/// a script from before the window was the default leaves out `--here`.
pub fn running_in_test_pty() -> bool {
    std::env::var_os("TERM_PROGRAM").is_some_and(|value| value == "ptyshot")
}

/// Whether this process was started over SSH (`SSH_CONNECTION` or `SSH_TTY`), where a
/// window would open on the far Mac's screen, out of sight.
pub fn running_over_ssh() -> bool {
    ["SSH_CONNECTION", "SSH_TTY"].iter().any(|name| is_set(std::env::var_os(name).as_deref()))
}

/// Whether a program rather than a shell started this process, from a terminal: git,
/// crontab or an agent running its `$EDITOR`, which reads the file back once bakedit
/// returns. `tty` says whether there is a terminal; without one nothing is waiting to edit.
pub fn started_by_program(tty: bool) -> bool {
    tty && parent_name().is_some_and(|name| !is_shell(&name))
}

/// The command name of the parent process, asked of the kernel (no `ps` to run, which a
/// sandbox may forbid); `None` when it cannot be told.
#[cfg(target_os = "macos")]
fn parent_name() -> Option<String> {
    let parent = libc::pid_t::try_from(std::os::unix::process::parent_id()).ok()?;
    let mut buffer = [0u8; 256];
    // SAFETY: proc_name writes at most `buffer.len()` bytes into `buffer`, which lives for
    // the call, and returns how many it wrote (0 or less on failure).
    let written = unsafe { libc::proc_name(parent, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    let written = usize::try_from(written).ok().filter(|&n| n > 0 && n <= buffer.len())?;
    Some(String::from_utf8_lossy(&buffer[..written]).into_owned())
}

/// The command name of the parent process, from `/proc`; `None` when it cannot be told.
#[cfg(not(target_os = "macos"))]
fn parent_name() -> Option<String> {
    let name = std::fs::read_to_string(format!("/proc/{}/comm", std::os::unix::process::parent_id())).ok()?;
    Some(name.trim().to_string()).filter(|name| !name.is_empty())
}

/// Whether `comm`, a process's command name (a path, or `-zsh` for a login shell, are
/// taken too), is an interactive shell's.
fn is_shell(comm: &str) -> bool {
    let name = comm.rsplit('/').next().unwrap_or(comm).trim_start_matches('-');
    const SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "ash", "ksh", "mksh", "oksh", "tcsh", "csh", "yash", "nu", "xonsh", "elvish", "pwsh", "ion", "osh"];
    SHELLS.contains(&name)
}

/// A variable counts when it is set to anything but the empty string.
fn is_set(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// What [`decide`] needs besides the command line, gathered by `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// The `"window"` setting.
    pub window_setting: bool,
    /// This is the bakedit a dedicated window runs ([`running_in_window`]).
    pub in_window: bool,
    /// This bakedit runs under test, in ptyshot ([`running_in_test_pty`]).
    pub test_pty: bool,
    /// Started over SSH ([`running_over_ssh`]).
    pub ssh: bool,
    /// Ghostty is installed ([`find_ghostty`]).
    pub ghostty: bool,
    /// A running bakedit window would take the paths. Only looked for when
    /// [`may_hand_over`] says they could go to one.
    pub running_window: bool,
    /// Standard input and output are a terminal.
    pub tty: bool,
    /// A program rather than a shell started bakedit from a terminal ([`started_by_program`]):
    /// it is waiting for its editor.
    pub started_by_program: bool,
}

/// Where a `bakedit` command runs: [`decide`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// Hand the files to the bakedit window that is running, and exit.
    RunningWindow,
    /// Open a new dedicated Ghostty window running bakedit, and exit at once, or when the
    /// window closes if bakedit [`waits`].
    NewWindow,
    /// Run in the current terminal. `why` says why there is no window when one was
    /// wanted, to be said once: in the status bar, or on stderr before a diagnostic.
    Here { why: Option<String> },
    /// Nowhere to run: no window, and no terminal to run in. The text says why.
    Nowhere(String),
}

const NO_GHOSTTY: &str = "No Ghostty.app in /Applications or ~/Applications";
const NO_TERMINAL: &str = "bakedit needs a terminal to run in (for a running window use --action, or pass files without --new)";

/// Where `bakedit` runs for `args`, the first rule that applies:
///
/// 1. `--window` opens a new window, even with `"window": false`, over SSH or with a window
///    running; but the bakedit a window runs, and one under test, never open a window,
///    whatever they are given.
/// 2. `--dump-input` and `--probe` run here (each checks for a terminal itself).
/// 3. Files, without `--new`, go to the running window that takes them, unless bakedit
///    [`waits`]: a running window cannot say when the file is closed.
/// 4. `--here`, `"window": false`, an SSH session, the bakedit a window runs and one
///    under test run here.
/// 5. Anything else opens a new window.
///
/// A window needs Ghostty; without it bakedit runs here and says why. A window needs no
/// terminal, so a script or an agent's hook can open one. Running here does: without a
/// terminal the answer is [`Place::Nowhere`].
pub fn decide(args: &Args, facts: &Facts) -> Place {
    let diagnostic = is_diagnostic(args);
    let never_a_window = facts.in_window || facts.test_pty;
    if args.window == Some(true) && !never_a_window {
        return window_unless_missing(facts, diagnostic);
    }
    if diagnostic {
        return Place::Here { why: None };
    }
    if facts.running_window && !args.new_instance && !args.paths.is_empty() && !waits(args, facts) {
        return Place::RunningWindow;
    }
    if never_a_window || facts.ssh || args.window == Some(false) || !facts.window_setting {
        return if facts.tty { Place::Here { why: None } } else { Place::Nowhere(NO_TERMINAL.to_string()) };
    }
    window_unless_missing(facts, false)
}

/// Whether bakedit must not return before its files are closed: `--wait`, or, unless
/// `--no-wait`, when a program started it ([`Facts::started_by_program`]), such as git
/// running `$EDITOR`. Such a bakedit runs here or in a new window it waits for
/// (`open -W`), never in a running window. Only editing waits.
pub fn waits(args: &Args, facts: &Facts) -> bool {
    args.mode == Mode::Edit && args.wait.unwrap_or(facts.started_by_program)
}

/// Where to run after [`decide`] said [`Place::NewWindow`] and opening it failed with
/// `error`: here, saying why, when there is a terminal (a diagnostic checks for its own).
pub fn after_failed_launch(error: &str, args: &Args, facts: &Facts) -> Place {
    instead_of_window(format!("Could not open a Ghostty window ({error})"), "", facts.tty || is_diagnostic(args))
}

/// Whether the files in `args` could go to a running window, so that it is worth looking
/// for one: editing, without `--new`, and only files (a directory is a project of its own).
pub fn may_hand_over(args: &Args) -> bool {
    args.mode == Mode::Edit && !args.new_instance && !args.paths.is_empty() && args.paths.iter().all(|spec| !spec.path.is_dir())
}

fn is_diagnostic(args: &Args) -> bool {
    matches!(args.mode, Mode::DumpInput | Mode::Probe)
}

fn window_unless_missing(facts: &Facts, diagnostic: bool) -> Place {
    if facts.ghostty {
        Place::NewWindow
    } else {
        instead_of_window(NO_GHOSTTY.to_string(), " (\"window\": false stops looking)", facts.tty || diagnostic)
    }
}

/// A window was wanted but there is none, for `reason`: run here, saying so (and `hint`),
/// when bakedit `can_run_here`.
fn instead_of_window(reason: String, hint: &str, can_run_here: bool) -> Place {
    if can_run_here {
        Place::Here { why: Some(format!("{reason}: running in this terminal{hint}")) }
    } else {
        Place::Nowhere(format!("{reason}, and no terminal to run in"))
    }
}

/// How long a new window and the `open` that starts it last.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hold {
    /// Keep the window up after bakedit exits, until a key is pressed, so a diagnostic's
    /// output can be read.
    pub keep_open: bool,
    /// `open` returns only when that Ghostty instance quits (`-W`), which it does when its
    /// one window closes (`quit-after-last-window-closed`): for a bakedit that [`waits`].
    pub wait: bool,
}

/// The command that opens the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// Variables removed from the environment `open` passes on to the new process.
    pub env_remove: Vec<String>,
}

impl WindowCommand {
    /// The command line, quoted for a shell, for `--print-ghostty-config` and messages.
    pub fn display(&self) -> String {
        let quote = |text: &str| {
            if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
                text.to_string()
            } else {
                format!("'{}'", text.replace('\'', r"'\''"))
            }
        };
        let mut parts: Vec<String> = Vec::new();
        if !self.env_remove.is_empty() {
            parts.push("env".to_string());
            for name in &self.env_remove {
                parts.push(format!("-u {}", quote(name)));
            }
        }
        parts.push(quote(&self.program.to_string_lossy()));
        parts.extend(self.args.iter().map(|arg| quote(&arg.to_string_lossy())));
        parts.join(" ")
    }
}

/// The names in `names` the window's process must not inherit.
pub fn scrubbed(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = names.into_iter().filter(|name| SCRUBBED_PREFIXES.iter().any(|prefix| name.starts_with(prefix))).collect();
    out.sort();
    out.dedup();
    out
}

/// `<flag><path>` as one argument, whatever bytes the path holds.
fn flag_with_path(flag: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(flag);
    arg.push(path.as_os_str());
    arg
}

/// The `open` command for a window: Ghostty at `ghostty`, its config from `entry`, starting
/// in `cwd`, running `bakedit --here --new --cwd=<cwd>` followed by `forward`, the user's
/// arguments without the launcher's own flags ([`Args::forward`]). `--here` (with the
/// `BAKEDIT_HOST` the config sets) stops that bakedit from opening another window, and
/// `--new` from handing its files to another window. `hold` says how long the window and
/// `open` last ([`Hold`]). `env_names` are the variables of this process.
///
/// The arguments arrive whole, spaces and all: `open --args` hands them to Ghostty, `-e`
/// makes the rest the command, run without a shell (Ghostty 1.4.0
/// `src/config/Config.zig:4996`), through `/usr/bin/login -flp <user> <command...>`, which
/// keeps the environment and the directory (`src/termio/Exec.zig:1933-1942`). The
/// directory goes twice: `--working-directory` starts the window there, but Ghostty reads
/// the user's own config after the command line (`macos/Sources/Ghostty/Ghostty.Config.swift:77-80`),
/// so a `working-directory` line in it would win, and it trims the value
/// (`src/config/Config.zig:5572`); `--cwd` tells bakedit itself, so relative paths still
/// mean what they meant here.
pub fn window_command(ghostty: &Path, entry: &Path, cwd: &Path, bakedit: &Path, forward: &[String], env_names: impl IntoIterator<Item = String>, hold: Hold) -> WindowCommand {
    let mut args: Vec<OsString> = vec![if hold.wait { "-Wna" } else { "-na" }.into(), ghostty.into(), "--args".into(), "--config-default-files=false".into()];
    args.push(flag_with_path("--config-file=", entry));
    args.push(flag_with_path("--working-directory=", cwd));
    if hold.keep_open {
        args.push("--wait-after-command=true".into());
    }
    // `-e` takes the rest of the line as the command, so it goes last.
    args.push("-e".into());
    args.push(bakedit.into());
    args.push("--here".into());
    args.push("--new".into());
    args.push(flag_with_path("--cwd=", cwd));
    args.extend(forward.iter().map(OsString::from));
    WindowCommand { program: PathBuf::from("/usr/bin/open"), args, env_remove: scrubbed(env_names) }
}

/// Write the config and open a window running bakedit in `cwd` with `forward`, Ghostty
/// being at `ghostty` (see [`window_command`]). Returns once `open` has handed the window
/// to Ghostty, and the window runs on its own; with [`Hold::wait`], once it has closed. The error is short, for the status bar; the log
/// has the command line.
pub fn open_window(ghostty: &Path, keymap: &Keymap, cwd: &Path, forward: &[String], hold: Hold) -> Result<(), String> {
    let entry = write_config(&config_dir(), &ghostty_config(keymap)).map_err(|error| format!("could not write the Ghostty config: {error}"))?;
    let bakedit = std::env::current_exe().map_err(|error| format!("cannot tell where bakedit is: {error}"))?;
    let names = std::env::vars_os().filter_map(|(name, _)| name.into_string().ok());
    let command = window_command(ghostty, &entry, cwd, &bakedit, forward, names, hold);
    log::info!("opening a Ghostty window: {}", command.display());
    let mut process = Command::new(&command.program);
    process.args(&command.args);
    for name in &command.env_remove {
        process.env_remove(name);
    }
    let status = process.status().map_err(|error| format!("could not run open: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("open failed, {status}"))
    }
}

/// What `--print-ghostty-config` prints: both files under their paths, and the command
/// that opens a window with them.
pub fn describe(keymap: &Keymap) -> String {
    let config = ghostty_config(keymap);
    let dir = config_dir();
    let ghostty = find_ghostty().unwrap_or_else(|| PathBuf::from("/Applications/Ghostty.app"));
    let bakedit = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("bakedit"));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let names = std::env::vars_os().filter_map(|(name, _)| name.into_string().ok());
    let command = window_command(&ghostty, &dir.join("bakedit.ghostty"), &cwd, &bakedit, &[], names, Hold::default());
    format!(
        "# {entry_path}\n{entry}\n# {window_path}\n{window}\n# bakedit (or with \"window\": false, bakedit --window) writes these files and runs:\n#   {command}\n",
        entry_path = dir.join("bakedit.ghostty").display(),
        entry = config.entry,
        window_path = dir.join("bakedit-window.ghostty").display(),
        window = config.window,
        command = command.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bakedit_keymap::KeymapSource;

    fn keymap(extra: Option<&str>) -> Keymap {
        let mut keymap = Keymap::new();
        keymap.load(crate::keys::DEFAULT_KEYMAP, KeymapSource::Default);
        if let Some(extra) = extra {
            keymap.load(extra, KeymapSource::User);
        }
        keymap
    }

    #[test]
    fn the_window_config_forwards_every_chord() {
        let config = ghostty_config(&keymap(None));
        let lines: Vec<&str> = config.window.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).collect();
        for wanted in [
            "keybind = clear",
            "keybind = super+==increase_font_size:1",
            "keybind = performable:super+v=paste_from_clipboard",
            "macos-option-as-alt = left",
            "mouse-shift-capture = true",
            "link-url = false",
            "quit-after-last-window-closed = true",
            "shell-integration = none",
            "env = BAKEDIT_HOST=ghostty-dedicated",
        ] {
            assert!(lines.contains(&wanted), "missing {wanted:?}");
        }
        assert_eq!(lines.iter().filter(|l| l.starts_with("keybind = clear")).count(), 1);
        assert_eq!(lines.iter().position(|l| *l == "keybind = clear"), Some(2), "clear comes before the allow-list");
        // Every line is `key = value`, which is all a Ghostty config line can be.
        assert!(lines.iter().all(|l| l.split_once(" = ").is_some()), "{lines:?}");
    }

    #[test]
    fn menu_chords_are_forwarded_only_when_the_keymap_uses_them() {
        let plain = ghostty_config(&keymap(None)).window;
        assert!(plain.contains("# keybind = super+h=csi:104;9u"), "unused: left to macOS");
        assert!(plain.contains("# keybind = super+m=csi:109;9u"));
        let bound = ghostty_config(&keymap(Some(r#"[{ "bindings": { "cmd-h": "workspace::ToggleLeftDock" } }]"#))).window;
        assert!(bound.lines().any(|l| l == "keybind = super+h=csi:104;9u"), "used: handed to bakedit");
        assert!(bound.contains("# keybind = super+m=csi:109;9u"));
        let switched_off = ghostty_config(&keymap(Some(r#"[{ "bindings": { "cmd-h": null } }]"#))).window;
        assert!(switched_off.contains("# keybind = super+h"), "a null binding is not a use");
    }

    #[test]
    fn the_entry_file_puts_the_users_config_first_and_ours_last() {
        let entry = ghostty_config(&keymap(None)).entry;
        let includes: Vec<&str> = entry.lines().filter(|l| l.starts_with("config-file")).collect();
        assert_eq!(includes.get(includes.len() - 2), Some(&"config-file = bakedit-window.ghostty"));
        assert_eq!(includes.last(), Some(&"config-file = ?bakedit-user.ghostty"));
        assert!(includes[..includes.len() - 2].iter().all(|l| l.starts_with("config-file = ?")), "the user's files are optional");
    }

    #[test]
    fn the_files_are_written_into_our_own_folder_and_only_when_changed() {
        let dir = tempfile::tempdir().unwrap();
        let config = ghostty_config(&keymap(None));
        let entry = write_config(dir.path(), &config).unwrap();
        assert_eq!(entry, dir.path().join("bakedit.ghostty"));
        assert_eq!(std::fs::read_to_string(dir.path().join("bakedit-window.ghostty")).unwrap(), config.window);
        let stamp = std::fs::metadata(&entry).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_config(dir.path(), &config).unwrap();
        assert_eq!(std::fs::metadata(&entry).unwrap().modified().unwrap(), stamp, "unchanged text is not rewritten");
        assert!(config_dir().ends_with("bakedit/ghostty"), "never the user's Ghostty folder");
    }

    #[test]
    fn the_launch_command_follows_the_research() {
        let names = ["PATH", "HOME", "HERDR_ENV", "HERDR_SOCKET_PATH", "HERDR_PANE_ID", "TMUX", "TMUX_PANE", "ZELLIJ", "TERM"].map(String::from);
        let forward = crate::cli::parse(["--window", "src/main.rs:12"], Path::new("/home/me/Code/app")).unwrap().forward;
        let command = window_command(
            Path::new("/Applications/Ghostty.app"),
            Path::new("/home/me/.config/bakedit/ghostty/bakedit.ghostty"),
            Path::new("/home/me/Code/app"),
            Path::new("/usr/local/bin/bakedit"),
            &forward,
            names,
            Hold::default(),
        );
        let args: Vec<String> = command.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(command.program, PathBuf::from("/usr/bin/open"), "found without PATH, as a hook may have none");
        assert_eq!(
            args,
            [
                "-na",
                "/Applications/Ghostty.app",
                "--args",
                "--config-default-files=false",
                "--config-file=/home/me/.config/bakedit/ghostty/bakedit.ghostty",
                "--working-directory=/home/me/Code/app",
                "-e",
                "/usr/local/bin/bakedit",
                "--here",
                "--new",
                "--cwd=/home/me/Code/app",
                "src/main.rs:12",
            ],
            "no -g (it would never open the window); -e last; --window is not passed on; the window's bakedit runs where it is"
        );
        assert_eq!(command.env_remove, ["HERDR_ENV", "HERDR_PANE_ID", "HERDR_SOCKET_PATH", "TMUX", "TMUX_PANE", "ZELLIJ"]);
        let shown = command.display();
        assert!(shown.starts_with("env -u HERDR_ENV -u HERDR_PANE_ID"), "{shown}");
        assert!(shown.contains(" -e /usr/local/bin/bakedit --here --new --cwd=/home/me/Code/app src/main.rs:12"), "{shown}");
    }

    #[test]
    fn spaces_and_relative_paths_arrive_whole() {
        let cwd = Path::new("/Users/me/My Projects/app");
        let line = ["notes/to do.md:3", "--terminal", "herdr --session work", "--log", "debug log.txt", "--", "-odd name"];
        let forward = crate::cli::parse(line, cwd).unwrap().forward;
        let command = window_command(Path::new("/Applications/Ghostty.app"), Path::new("/c/bakedit.ghostty"), cwd, Path::new("/bin/bakedit"), &forward, Vec::new(), Hold::default());
        let e = command.args.iter().position(|a| a == "-e").unwrap();
        // One argument each, exactly as typed: no shell splits them on the way.
        let after: Vec<&OsStr> = command.args[e + 1..].iter().map(OsString::as_os_str).collect();
        assert_eq!(
            after,
            ["/bin/bakedit", "--here", "--new", "--cwd=/Users/me/My Projects/app", "notes/to do.md:3", "--terminal", "herdr --session work", "--log", "debug log.txt", "--", "-odd name"]
        );
        assert!(command.args.contains(&OsString::from("--working-directory=/Users/me/My Projects/app")));
        // The bakedit in the window reads the line back into the same files, positions and options.
        let inner: Vec<String> = after[1..].iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let theirs = crate::cli::parse(inner, Path::new("/somewhere/else")).unwrap();
        let ours = crate::cli::parse(line, cwd).unwrap();
        assert_eq!(theirs.paths, ours.paths);
        assert_eq!((theirs.terminal_command, theirs.log_file), (ours.terminal_command, ours.log_file));
        assert_eq!((theirs.window, theirs.new_instance), (Some(false), true));
        assert_eq!(theirs.forward, ours.forward, "and would pass on the same, were it to");
        assert!(command.display().contains(" '--cwd=/Users/me/My Projects/app' 'notes/to do.md:3' --terminal 'herdr --session work'"), "{}", command.display());
    }

    #[test]
    fn a_diagnostic_window_stays_open_to_be_read() {
        let command = |keep_open| window_command(Path::new("/G.app"), Path::new("/c/b.ghostty"), Path::new("/w"), Path::new("/b"), &["--probe".to_string()], Vec::new(), Hold { keep_open, wait: false }).args;
        let kept = command(true);
        let e = kept.iter().position(|a| a == "-e").unwrap();
        assert_eq!(kept[e - 1], "--wait-after-command=true", "a Ghostty option, before -e");
        assert_eq!(kept[e..].last().map(OsString::as_os_str), Some(OsStr::new("--probe")));
        assert!(!command(false).contains(&OsString::from("--wait-after-command=true")));
    }

    #[test]
    fn a_waiting_launch_returns_when_the_window_closes() {
        let command = |wait| window_command(Path::new("/G.app"), Path::new("/c/b.ghostty"), Path::new("/w"), Path::new("/b"), &["COMMIT_EDITMSG".to_string()], Vec::new(), Hold { keep_open: false, wait }).args;
        assert_eq!(command(true)[0], "-Wna", "open -W waits for the new instance, which quits with its window");
        assert_eq!(command(false)[0], "-na");
        let waiting = command(true);
        let e = waiting.iter().position(|a| a == "-e").unwrap();
        assert_eq!(waiting[e + 1..].iter().filter(|a| a.to_string_lossy().contains("wait")).count(), 0, "the window's bakedit is not told to wait");
        assert!(ghostty_config(&keymap(None)).window.contains("\nquit-after-last-window-closed = true\n"), "or open -W would never return");
    }

    #[test]
    fn a_shell_is_told_from_a_program() {
        for shell in ["zsh", "-zsh", "/bin/zsh", "bash", "-bash", "/bin/sh", "fish", "/opt/homebrew/bin/nu", "dash"] {
            assert!(is_shell(shell), "{shell}");
        }
        for program in ["git", "/usr/bin/crontab", "kubectl", "node", "claude", "login", "sudo", "zshell"] {
            assert!(!is_shell(program), "{program}");
        }
        assert!(!started_by_program(false), "without a terminal nothing is waiting to edit");
    }

    fn facts() -> Facts {
        Facts { window_setting: true, in_window: false, test_pty: false, ssh: false, ghostty: true, running_window: false, tty: true, started_by_program: false }
    }

    fn decided(line: &[&str], facts: Facts) -> Place {
        decide(&crate::cli::parse(line.iter().copied(), Path::new("/work")).unwrap(), &facts)
    }

    fn here(why: Option<&str>) -> Place {
        Place::Here { why: why.map(str::to_string) }
    }

    #[test]
    fn a_plain_bakedit_opens_a_window() {
        assert_eq!(decided(&[], facts()), Place::NewWindow);
        assert_eq!(decided(&["src/main.rs:3"], facts()), Place::NewWindow, "no window running to take it");
        assert_eq!(decided(&["src"], facts()), Place::NewWindow);
        assert_eq!(decided(&["--terminal", "shell", "--no-restore"], facts()), Place::NewWindow);
        // From a script or an agent's hook: a window needs no terminal.
        assert_eq!(decided(&["src/main.rs:3"], Facts { tty: false, ..facts() }), Place::NewWindow);
    }

    #[test]
    fn bakedit_as_editor_returns_only_when_the_file_is_closed() {
        // `GIT_EDITOR=bakedit git commit`: git runs `bakedit .git/COMMIT_EDITMSG` and reads the
        // file back as soon as bakedit returns.
        let git = Facts { started_by_program: true, ..facts() };
        let line = ["/repo/.git/COMMIT_EDITMSG"];
        let args = crate::cli::parse(line, Path::new("/repo")).unwrap();
        assert_eq!(decided(&line, git), Place::NewWindow, "no window running: a new one");
        assert!(waits(&args, &git), "and open -W waits for it to close");
        let running = Facts { running_window: true, ..git };
        assert_eq!(decided(&line, running), Place::NewWindow, "a running window cannot say when the file is closed");
        assert_eq!(decided(&["--here", "/repo/.git/COMMIT_EDITMSG"], running), here(None), "EDITOR=\"bakedit --here\" edits in git's terminal");
        assert_eq!(decided(&line, Facts { window_setting: false, ..running }), here(None));
        assert_eq!(decided(&line, Facts { ghostty: false, ..running }), here(Some("No Ghostty.app in /Applications or ~/Applications: running in this terminal (\"window\": false stops looking)")));
        // From a shell nothing waits, as before; --wait and --no-wait say otherwise.
        let shell = facts();
        assert!(!waits(&args, &shell));
        assert_eq!(decided(&line, Facts { running_window: true, ..shell }), Place::RunningWindow);
        assert_eq!(decided(&["--wait", "/repo/.git/COMMIT_EDITMSG"], Facts { running_window: true, ..shell }), Place::NewWindow);
        assert!(waits(&crate::cli::parse(["--wait", "a.rs"], Path::new("/repo")).unwrap(), &shell));
        assert!(!waits(&crate::cli::parse(["--no-wait", "a.rs"], Path::new("/repo")).unwrap(), &git));
        assert_eq!(decided(&["--no-wait", "/repo/.git/COMMIT_EDITMSG"], running), Place::RunningWindow);
        assert!(!waits(&crate::cli::parse(["--probe"], Path::new("/repo")).unwrap(), &git), "only editing waits");
    }

    #[test]
    fn files_go_to_the_running_window_first() {
        let running = Facts { running_window: true, ..facts() };
        assert_eq!(decided(&["src/main.rs:3"], running), Place::RunningWindow);
        assert_eq!(decided(&["src/main.rs:3"], Facts { tty: false, ..running }), Place::RunningWindow, "an agent's hook");
        assert_eq!(decided(&["--here", "src/main.rs:3"], running), Place::RunningWindow, "as bakedit always did here");
        assert_eq!(decided(&["src/main.rs:3"], Facts { window_setting: false, ..running }), Place::RunningWindow);
        assert_eq!(decided(&["--new", "src/main.rs:3"], running), Place::NewWindow, "--new: a window of its own");
        assert_eq!(decided(&["--new", "--here", "src/main.rs:3"], running), here(None));
        assert_eq!(decided(&["--window", "src/main.rs:3"], running), Place::NewWindow, "--window: always a new window");
        assert_eq!(decided(&[], running), Place::NewWindow, "no files to hand over");
    }

    #[test]
    fn here_by_flag_or_setting() {
        assert_eq!(decided(&["--here"], facts()), here(None));
        assert_eq!(decided(&["--no-window", "a.rs"], facts()), here(None));
        assert_eq!(decided(&[], Facts { window_setting: false, ..facts() }), here(None));
        assert_eq!(decided(&["--window"], Facts { window_setting: false, ..facts() }), Place::NewWindow, "--window wins over the setting");
        assert_eq!(decided(&["--window", "--here"], facts()), here(None), "the last flag wins");
        assert_eq!(decided(&["--here", "--window"], facts()), Place::NewWindow);
        let Place::Nowhere(why) = decided(&["--here"], Facts { tty: false, ..facts() }) else { panic!("no terminal to run in") };
        assert!(why.contains("needs a terminal"), "{why}");
        assert!(matches!(decided(&[], Facts { tty: false, window_setting: false, ..facts() }), Place::Nowhere(_)));
    }

    #[test]
    fn the_bakedit_a_window_runs_never_opens_another() {
        let inside = Facts { in_window: true, ..facts() };
        assert_eq!(decided(&["--here", "--new", "--cwd=/work", "a.rs"], inside), here(None));
        assert_eq!(decided(&[], inside), here(None), "even without --here");
        assert_eq!(decided(&["--window"], inside), here(None), "even when told to");
        assert_eq!(decided(&["--window", "--probe"], inside), here(None));
        assert_eq!(decided(&["a.rs"], Facts { running_window: true, ..inside }), Place::RunningWindow, "files still go to a running window, as with --here");
        assert!(is_set(Some(OsStr::new(HOST_NAME))));
        assert!(!is_set(Some(OsStr::new(""))), "an empty BAKEDIT_HOST is no host");
        assert!(!is_set(None));
    }

    #[test]
    fn a_bakedit_under_test_never_opens_a_window() {
        // ptyshot, even driven by a script from before --here existed.
        let tested = Facts { test_pty: true, ..facts() };
        assert_eq!(decided(&["--terminal", "shell", "--new", "/work/demo"], tested), here(None));
        assert_eq!(decided(&["--window"], tested), here(None));
        assert_eq!(decided(&["--window", "--probe"], tested), here(None));
        assert_eq!(decided(&["a.rs"], Facts { running_window: true, ..tested }), Place::RunningWindow);
    }

    #[test]
    fn over_ssh_it_takes_window_to_open_one() {
        let remote = Facts { ssh: true, ..facts() };
        assert_eq!(decided(&[], remote), here(None), "a window would open out of sight");
        assert_eq!(decided(&["a.rs"], remote), here(None));
        assert_eq!(decided(&["a.rs"], Facts { running_window: true, ..remote }), Place::RunningWindow);
        assert_eq!(decided(&["--window"], remote), Place::NewWindow, "asked for, so opened");
        assert!(matches!(decided(&[], Facts { tty: false, ..remote }), Place::Nowhere(_)));
    }

    #[test]
    fn without_ghostty_bakedit_runs_here_and_says_why() {
        let missing = Facts { ghostty: false, ..facts() };
        let lines: [&[&str]; 3] = [&[], &["--window"], &["a.rs"]];
        for line in lines {
            let Place::Here { why: Some(why) } = decided(line, missing) else { panic!("{line:?} runs here") };
            assert!(why.starts_with("No Ghostty.app in /Applications") && why.contains("running in this terminal") && why.contains("\"window\": false"), "{why}");
        }
        assert_eq!(decided(&["--here"], missing), here(None), "nothing to say when no window was wanted");
        let Place::Nowhere(why) = decided(&["a.rs"], Facts { tty: false, ..missing }) else { panic!("no window and no terminal") };
        assert_eq!(why, "No Ghostty.app in /Applications or ~/Applications, and no terminal to run in");
    }

    #[test]
    fn a_failed_launch_falls_back_to_this_terminal() {
        let args = crate::cli::parse(["a.rs"], Path::new("/work")).unwrap();
        assert_eq!(
            after_failed_launch("open failed, exit status: 1", &args, &facts()),
            here(Some("Could not open a Ghostty window (open failed, exit status: 1): running in this terminal"))
        );
        assert_eq!(
            after_failed_launch("open failed, exit status: 1", &args, &Facts { tty: false, ..facts() }),
            Place::Nowhere("Could not open a Ghostty window (open failed, exit status: 1), and no terminal to run in".into())
        );
        let probe = crate::cli::parse(["--window", "--probe"], Path::new("/work")).unwrap();
        assert!(matches!(after_failed_launch("x", &probe, &Facts { tty: false, ..facts() }), Place::Here { why: Some(_) }), "a diagnostic checks for its own terminal");
    }

    #[test]
    fn diagnostics_run_here_unless_told_otherwise() {
        for diagnostic in ["--probe", "--dump-input"] {
            assert_eq!(decided(&[diagnostic], facts()), here(None));
            assert_eq!(decided(&[diagnostic], Facts { running_window: true, ..facts() }), here(None));
            assert_eq!(decided(&[diagnostic], Facts { tty: false, ..facts() }), here(None), "it says itself that it needs a terminal");
            assert_eq!(decided(&["--window", diagnostic], facts()), Place::NewWindow);
            assert_eq!(decided(&[diagnostic, "--window"], Facts { window_setting: false, ..facts() }), Place::NewWindow);
            assert!(matches!(decided(&["--window", diagnostic], Facts { ghostty: false, tty: false, ..facts() }), Place::Here { why: Some(_) }));
        }
    }

    #[test]
    fn only_files_can_go_to_a_running_window() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        let may = |line: &[&str]| may_hand_over(&crate::cli::parse(line.iter().copied(), dir.path()).unwrap());
        assert!(may(&["a.rs:3"]));
        assert!(may(&["missing.rs"]), "a file to be made");
        assert!(may(&["--here", "a.rs"]));
        assert!(!may(&[]), "nothing to hand over");
        assert!(!may(&["sub"]), "a directory is a project of its own");
        assert!(!may(&["a.rs", "sub"]));
        assert!(!may(&["--new", "a.rs"]));
        assert!(!may(&["--probe", "a.rs"]));
    }

    #[test]
    fn the_window_never_saves_or_restores_state() {
        let config = ghostty_config(&keymap(None)).window;
        assert!(config.lines().any(|line| line == "window-save-state = never"), "see the comment above it in the file");
    }

    #[test]
    fn quoting_for_display() {
        let command = WindowCommand { program: "open".into(), args: vec!["a b".into(), "it's".into(), "plain".into()], env_remove: Vec::new() };
        assert_eq!(command.display(), r"open 'a b' 'it'\''s' plain");
    }
}
