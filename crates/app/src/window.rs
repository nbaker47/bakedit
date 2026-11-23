//! `bakedit --window`: bakedit in a Ghostty window of its own, set up so that every cmd
//! chord reaches bakedit instead of Ghostty (new tab, close, quit, splits, search...).
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
//! The window is opened with `open -na Ghostty.app --args ... -e bakedit --new ...` from the
//! current environment minus what would mislead the new process: herdr's `HERDR_*`
//! variables (it is no longer inside a herdr pane, so its terminal panel should run herdr),
//! and those of terminal multiplexers.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use bakedit_keymap::{Keymap, Keystroke};

/// Bumped when the generated files change in a way that matters; written into the window
/// so bakedit can tell an out-of-date window from a current one (`BAKEDIT_HOST_CONFIG`).
pub const CONFIG_VERSION: u32 = 1;

/// The environment value the window gives bakedit, so it knows every cmd chord is coming.
pub const HOST_NAME: &str = "ghostty-dedicated";

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
# Never save or restore windows in this instance, so it cannot bring back your
# ordinary Ghostty windows. Ghostty keeps this preference for every instance
# until your own Ghostty starts again and resets it.
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

/// The `open` command for a window: Ghostty at `ghostty`, its config from `entry`, starting
/// in `cwd`, running `bakedit` with `bakedit_args` after `--new` (the window must not hand
/// its files to another window). `env_names` are the variables of this process.
pub fn window_command(ghostty: &Path, entry: &Path, cwd: &Path, bakedit: &Path, bakedit_args: &[String], env_names: impl IntoIterator<Item = String>) -> WindowCommand {
    let mut args: Vec<OsString> = vec!["-na".into(), ghostty.into(), "--args".into(), "--config-default-files=false".into()];
    args.push(format!("--config-file={}", entry.display()).into());
    args.push(format!("--working-directory={}", cwd.display()).into());
    // `-e` takes the rest of the line as the command, so it goes last.
    args.push("-e".into());
    args.push(bakedit.into());
    args.push("--new".into());
    args.extend(bakedit_args.iter().filter(|arg| *arg != "--window" && *arg != "--new" && *arg != "-n").map(OsString::from));
    WindowCommand { program: PathBuf::from("open"), args, env_remove: scrubbed(env_names) }
}

/// Write the config and open the window. Returns at once; the window runs on its own.
pub fn open_window(keymap: &Keymap, bakedit_args: &[String]) -> Result<(), String> {
    let ghostty = find_ghostty().ok_or("Ghostty is not installed in /Applications: --window needs it")?;
    let entry = write_config(&config_dir(), &ghostty_config(keymap)).map_err(|error| format!("could not write the Ghostty config: {error}"))?;
    let cwd = std::env::current_dir().map_err(|error| format!("no working directory: {error}"))?;
    let bakedit = std::env::current_exe().map_err(|error| format!("cannot tell where bakedit is: {error}"))?;
    let names = std::env::vars_os().filter_map(|(name, _)| name.into_string().ok());
    let command = window_command(&ghostty, &entry, &cwd, &bakedit, bakedit_args, names);
    let mut process = Command::new(&command.program);
    process.args(&command.args);
    for name in &command.env_remove {
        process.env_remove(name);
    }
    let status = process.status().map_err(|error| format!("could not run open: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("open failed ({status}): {}", command.display()))
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
    let command = window_command(&ghostty, &dir.join("bakedit.ghostty"), &cwd, &bakedit, &[], names);
    format!(
        "# {entry_path}\n{entry}\n# {window_path}\n{window}\n# bakedit --window writes these files and runs:\n#   {command}\n",
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
        assert_eq!(includes.last(), Some(&"config-file = bakedit-window.ghostty"));
        assert!(includes[..includes.len() - 1].iter().all(|l| l.starts_with("config-file = ?")), "the user's files are optional");
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
        let command = window_command(
            Path::new("/Applications/Ghostty.app"),
            Path::new("/home/me/.config/bakedit/ghostty/bakedit.ghostty"),
            Path::new("/home/me/Code/app"),
            Path::new("/usr/local/bin/bakedit"),
            &["--window".to_string(), "src/main.rs:12".to_string()],
            names,
        );
        let args: Vec<String> = command.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(command.program, PathBuf::from("open"));
        assert_eq!(&args[..4], ["-na", "/Applications/Ghostty.app", "--args", "--config-default-files=false"], "no -g: it would never open the window");
        assert!(args.contains(&"--config-file=/home/me/.config/bakedit/ghostty/bakedit.ghostty".to_string()));
        assert!(args.contains(&"--working-directory=/home/me/Code/app".to_string()));
        let e = args.iter().position(|a| a == "-e").unwrap();
        assert_eq!(&args[e..], ["-e", "/usr/local/bin/bakedit", "--new", "src/main.rs:12"], "-e goes last, --window is not passed on");
        assert_eq!(command.env_remove, ["HERDR_ENV", "HERDR_PANE_ID", "HERDR_SOCKET_PATH", "TMUX", "TMUX_PANE", "ZELLIJ"]);
        let shown = command.display();
        assert!(shown.starts_with("env -u HERDR_ENV -u HERDR_PANE_ID"), "{shown}");
        assert!(shown.contains(" -e /usr/local/bin/bakedit --new src/main.rs:12"), "{shown}");
    }

    #[test]
    fn quoting_for_display() {
        let command = WindowCommand { program: "open".into(), args: vec!["a b".into(), "it's".into(), "plain".into()], env_remove: Vec::new() };
        assert_eq!(command.display(), r"open 'a b' 'it'\''s' plain");
    }
}
