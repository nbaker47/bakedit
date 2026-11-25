//! Floating bottom-center terminal-style Agent Dots overlay shown when leaving bakedit.
//!
//! - On **macOS**: compiles and runs a bundled terminal-styled floating `NSPanel` helper
//!   (`assets/macos/bakedit-dots/main.swift`) in `~/.local/state/bakedit/bin/bakedit-dots-overlay`,
//!   talking to it over a local Unix socket (`show` on `FocusLost`, `hide` on `FocusGained`,
//!   and `theme` updates whenever bakedit's theme changes).
//! - On **Linux**: spawns a compact borderless floating terminal (`ghostty`, `kitty`, `foot`,
//!   or `alacritty`) running `bakedit --dots` on `FocusLost`, and closes it on `FocusGained`.
//!
//! Never spawns anything during tests or inside `ptyshot`.

use std::path::{Path, PathBuf};
#[cfg(not(target_os = "macos"))]
use std::process::Child;
use std::process::{Command, Stdio};

use bakedit_core::Theme;
use serde_json::json;

#[cfg(target_os = "macos")]
const MACOS_OVERLAY_SWIFT: &str = include_str!("../../../../assets/macos/bakedit-dots/main.swift");

/// Controls the floating Agent Dots overlay lifecycle for a running bakedit window.
pub struct DotsOverlay {
    enabled: bool,
    state_dir: PathBuf,
    /// Currently showing the overlay (true when bakedit is unfocused and enabled).
    visible: bool,
    /// On Linux, the floating terminal child process (if spawned).
    #[cfg(not(target_os = "macos"))]
    child: Option<Child>,
    /// On macOS, whether we have already kicked off the background build/launch.
    #[cfg(target_os = "macos")]
    started: bool,
    /// Last theme JSON sent so we only push when the palette actually changes.
    last_theme_json: Option<String>,
}

impl DotsOverlay {
    /// Create a controller. Nothing is compiled or spawned until `set_enabled(true)` and
    /// `on_focus_changed(false)` occur (and never in test environments).
    pub fn new(state_dir: PathBuf, enabled: bool) -> DotsOverlay {
        DotsOverlay {
            enabled,
            state_dir,
            visible: false,
            #[cfg(not(target_os = "macos"))]
            child: None,
            #[cfg(target_os = "macos")]
            started: false,
            last_theme_json: None,
        }
    }

    /// Whether the overlay is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enable or disable the floating overlay. Disabling immediately hides it.
    pub fn set_enabled(&mut self, enabled: bool, theme: &Theme) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        if enabled {
            self.ensure_started(theme, false);
        } else {
            self.hide();
        }
    }

    /// Sync bakedit's active theme colours to the floating overlay.
    pub fn sync_theme(&mut self, theme: &Theme) {
        let payload = theme_command_json(theme);
        if self.last_theme_json.as_deref() == Some(payload.as_str()) {
            return;
        }
        self.last_theme_json = Some(payload.clone());
        if self.enabled && !suppressed() {
            #[cfg(target_os = "macos")]
            send_control_line(&self.control_socket(), &payload);
        }
    }

    /// Called when the bakedit window gains or loses OS focus.
    /// Leaving bakedit (`focused == false`) shows the bottom-center terminal-style bar;
    /// returning to bakedit (`focused == true`) hides it.
    pub fn on_focus_changed(&mut self, focused: bool, theme: &Theme) {
        if !self.enabled || suppressed() {
            return;
        }
        if focused {
            self.hide();
        } else {
            self.show(theme);
        }
    }

    /// Show the bottom-center overlay now.
    pub fn show(&mut self, theme: &Theme) {
        if suppressed() {
            return;
        }
        self.ensure_started(theme, true);
        self.visible = true;
        #[cfg(target_os = "macos")]
        {
            let sock = self.control_socket();
            if let Some(theme_json) = &self.last_theme_json {
                send_control_line(&sock, theme_json);
            }
            send_control_line(&sock, r#"{"cmd":"show"}"#);
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.spawn_linux_floating();
        }
    }

    /// Hide the floating overlay.
    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        #[cfg(target_os = "macos")]
        {
            send_control_line(&self.control_socket(), r#"{"cmd":"hide"}"#);
        }
        #[cfg(not(target_os = "macos"))]
        {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn ensure_started(&mut self, theme: &Theme, show_when_ready: bool) {
        if suppressed() {
            return;
        }
        self.last_theme_json = Some(theme_command_json(theme));
        #[cfg(target_os = "macos")]
        {
            if self.started {
                return;
            }
            self.started = true;
            let state_dir = self.state_dir.clone();
            let sock = self.control_socket();
            let theme_json = self.last_theme_json.clone();
            std::thread::Builder::new()
                .name("bakedit-dots-overlay-init".into())
                .spawn(move || {
                    if let Err(err) = ensure_macos_overlay_running(&state_dir, &sock, theme_json.as_deref(), show_when_ready) {
                        log::warn!("bakedit dots overlay: {err}");
                    }
                })
                .ok();
        }
    }

    #[cfg(target_os = "macos")]
    fn control_socket(&self) -> PathBuf {
        self.state_dir.join("dots-overlay.sock")
    }

    #[cfg(not(target_os = "macos"))]
    fn spawn_linux_floating(&mut self) {
        if let Some(child) = &mut self.child {
            if matches!(child.try_wait(), Ok(None)) {
                return;
            }
        }
        let Ok(exe) = std::env::current_exe() else { return };
        self.child = spawn_linux_terminal_for_dots(&exe);
    }
}

impl Drop for DotsOverlay {
    fn drop(&mut self) {
        self.hide();
    }
}

/// Never spawn external windows during tests or inside the ptyshot harness.
fn suppressed() -> bool {
    cfg!(test)
        || std::env::var_os("TERM_PROGRAM").is_some_and(|v| v == "ptyshot")
        || std::env::var_os("BAKEDIT_NO_OVERLAY").is_some()
}

/// Serialize bakedit's active theme colours for the terminal-styled floating overlay.
pub fn theme_command_json(theme: &Theme) -> String {
    json!({
        "cmd": "theme",
        "dark": theme.dark,
        "bg": theme.background.to_hex(),
        "surface": theme.surface.to_hex(),
        "elevated": theme.elevated_surface.to_hex(),
        "border": theme.border.to_hex(),
        "accent": theme.panel_focused_border.to_hex(),
        "text": theme.text.to_hex(),
        "muted": theme.text_muted.to_hex(),
        "green": theme.success.to_hex(),
        "yellow": theme.warning.to_hex(),
        "red": theme.error.to_hex(),
        "magenta": theme.ansi[5].to_hex(),
        "cyan": theme.ansi[6].to_hex(),
    })
    .to_string()
}

#[cfg(target_os = "macos")]
fn send_control_line(sock_path: &Path, line: &str) {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    if let Ok(mut stream) = UnixStream::connect(sock_path) {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
        let _ = writeln!(stream, "{line}");
    }
}

#[cfg(target_os = "macos")]
fn ensure_macos_overlay_running(
    state_dir: &Path,
    sock_path: &Path,
    theme_json: Option<&str>,
    show_when_ready: bool,
) -> Result<(), String> {
    use std::os::unix::net::UnixStream;

    let bin_dir = state_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| format!("mkdir {}: {e}", bin_dir.display()))?;

    let src_path = bin_dir.join("bakedit-dots-overlay.swift");
    let hash_path = bin_dir.join("bakedit-dots-overlay.hash");
    let bin_path = bin_dir.join("bakedit-dots-overlay");

    // Simple FNV-1a hash of the bundled Swift source so we only recompile when it changes.
    let hash = fnv1a64(MACOS_OVERLAY_SWIFT.as_bytes());
    let hash_str = format!("{hash:016x}");
    let up_to_date = bin_path.is_file()
        && std::fs::read_to_string(&hash_path).ok().as_deref().map(str::trim) == Some(hash_str.as_str());

    if !up_to_date {
        std::fs::write(&src_path, MACOS_OVERLAY_SWIFT).map_err(|e| format!("write swift: {e}"))?;
        // Remove the old binary first so macOS AMFI doesn't SIGKILL an overwritten inode.
        let _ = std::fs::remove_file(&bin_path);
        let status = Command::new("swiftc")
            .args(["-O", "-parse-as-library"])
            .arg(&src_path)
            .arg("-o")
            .arg(&bin_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("swiftc failed to run: {e}"))?;
        if !status.status.success() {
            let err = String::from_utf8_lossy(&status.stderr);
            return Err(format!("swiftc failed: {}", err.trim()));
        }
        let _ = std::fs::write(&hash_path, &hash_str);
        // If an older overlay daemon is already running, ask it to quit so we spawn the rebuilt one.
        if UnixStream::connect(sock_path).is_ok() {
            send_control_line(sock_path, r#"{"cmd":"quit"}"#);
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if UnixStream::connect(sock_path).is_err() {
                    break;
                }
            }
        }
    }

    // If an overlay daemon is already listening on the socket, just sync theme.
    if UnixStream::connect(sock_path).is_ok() {
        if let Some(theme_json) = theme_json {
            send_control_line(sock_path, theme_json);
        }
        if show_when_ready {
            send_control_line(sock_path, r#"{"cmd":"show"}"#);
        }
        return Ok(());
    }

    let _ = std::fs::remove_file(sock_path);
    let mut cmd = Command::new(&bin_path);
    cmd.arg("--control-socket").arg(sock_path);
    if !show_when_ready {
        cmd.arg("--start-hidden");
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn overlay: {e}"))?;

    // Wait briefly for the control socket to come up, then push the initial theme.
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if UnixStream::connect(sock_path).is_ok() {
            if let Some(theme_json) = theme_json {
                send_control_line(sock_path, theme_json);
            }
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// On Linux, launch a compact floating terminal running `bakedit --dots`.
#[cfg(not(target_os = "macos"))]
fn spawn_linux_terminal_for_dots(exe: &Path) -> Option<Child> {
    let candidates: &[(&str, &[&str])] = &[
        ("ghostty", &["--window-decoration=false", "--window-width=84", "--window-height=3", "-e"]),
        ("kitty", &["--class", "bakedit-dots", "--title", "bakedit-dots", "-o", "remember_window_size=no", "-o", "initial_window_width=84c", "-o", "initial_window_height=3c", "-e"]),
        ("foot", &["--app-id=bakedit-dots", "--window-size-chars=84x3", "-e"]),
        ("alacritty", &["--class", "bakedit-dots", "-o", "window.decorations=\"None\"", "-o", "window.dimensions.columns=84", "-o", "window.dimensions.lines=3", "-e"]),
    ];
    for (term, args) in candidates {
        if which_in_path(term) {
            if let Ok(child) = Command::new(term)
                .args(*args)
                .arg(exe)
                .arg("--dots")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                return Some(child);
            }
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn which_in_path(bin: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| dir.join(bin).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_command_json_contains_expected_keys() {
        let theme = Theme::builtin();
        let raw = theme_command_json(&theme);
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["cmd"], "theme");
        assert_eq!(parsed["bg"], "#12101a");
        assert_eq!(parsed["dark"], true);
    }

    #[test]
    fn overlay_is_suppressed_in_tests() {
        let dir = tempfile::tempdir().unwrap();
        let mut overlay = DotsOverlay::new(dir.path().to_path_buf(), true);
        let theme = Theme::builtin();
        overlay.on_focus_changed(false, &theme);
        overlay.on_focus_changed(true, &theme);
        assert!(overlay.is_enabled());
    }
}
