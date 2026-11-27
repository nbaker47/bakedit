//! Persistent host-window state: zoom level (font-size offset) and OS window position/size.
//!
//! Stored in `<state_dir>/window-state.json` so both zoom level and window geometry survive
//! restarts and reopenings across projects.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;
const MIN_ZOOM_OFFSET: i32 = -10;
const MAX_ZOOM_OFFSET: i32 = 30;

/// Saved OS window position and size in pixels.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub floating: bool,
}

/// Global window state persisted across runs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WindowState {
    #[serde(default)]
    pub version: u32,
    /// Font-size delta in points relative to the terminal's configured default.
    #[serde(default)]
    pub zoom_offset: i32,
    /// Last observed host window position and size.
    #[serde(default)]
    pub geometry: Option<WindowGeometry>,
}

/// Path to the persisted window state file.
pub fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("window-state.json")
}

/// Load the saved window state, falling back to defaults on missing, corrupt or newer files.
pub fn load(state_dir: &Path) -> WindowState {
    let Ok(bytes) = fs::read(state_path(state_dir)) else { return WindowState::default() };
    let Ok(mut state) = serde_json::from_slice::<WindowState>(&bytes) else {
        return WindowState::default();
    };
    if state.version > VERSION {
        return WindowState::default();
    }
    state.zoom_offset = state.zoom_offset.clamp(MIN_ZOOM_OFFSET, MAX_ZOOM_OFFSET);
    state
}

/// Atomically persist `state` under `state_dir`.
pub fn save(state_dir: &Path, state: &WindowState) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    let path = state_path(state_dir);
    let mut state = state.clone();
    state.version = VERSION;
    state.zoom_offset = state.zoom_offset.clamp(MIN_ZOOM_OFFSET, MAX_ZOOM_OFFSET);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(&state)?)?;
    fs::rename(&tmp, &path)
}

/// Apply a zoom delta string (`"+1"`, `"-1"`, `"0"`, etc.) to `current`.
pub fn apply_zoom_delta(current: i32, delta: &str) -> i32 {
    let trimmed = delta.trim();
    if trimmed == "0" {
        return 0;
    }
    if let Ok(step) = trimmed.parse::<i32>() {
        return (current + step).clamp(MIN_ZOOM_OFFSET, MAX_ZOOM_OFFSET);
    }
    current
}

/// Resolve the Kitty remote-control socket address for the current process, if any.
pub fn kitty_socket() -> Option<String> {
    std::env::var("KITTY_LISTEN_ON")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("KITTY_PID").ok().filter(|s| !s.is_empty()).map(|pid| format!("unix:/tmp/kitty-rc-{pid}")))
}

/// Send a `set-font-size` command to Kitty over `sock`.
#[cfg(not(test))]
pub fn send_kitty_font_size(sock: &str, value: &str) -> bool {
    let run = |bin: &str| {
        std::process::Command::new(bin)
            .args(["@", "--to", sock, "set-font-size", "--all", "--", value])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    run("kitty") || run("/Applications/kitty.app/Contents/MacOS/kitty")
}

/// Idempotently restore a persisted Kitty zoom offset (resetting to 0 first so restarting
/// inside an already-zoomed Kitty window never double-applies the offset).
#[cfg(not(test))]
pub fn restore_kitty_zoom(zoom_offset: i32) {
    if zoom_offset == 0 {
        return;
    }
    let Some(sock) = kitty_socket() else { return };
    if send_kitty_font_size(&sock, "0") {
        let delta = format!("{zoom_offset:+}");
        let _ = send_kitty_font_size(&sock, &delta);
    }
}

/// Parse `hyprctl clients -j` output and return the `(address, WindowGeometry)` of the first
/// client whose `pid` matches one of `ancestor_pids` (checked in priority order).
pub fn parse_hyprland_client(json_text: &str, ancestor_pids: &[u32]) -> Option<(String, WindowGeometry)> {
    let clients: serde_json::Value = serde_json::from_str(json_text).ok()?;
    let list = clients.as_array()?;
    for &wanted_pid in ancestor_pids {
        for client in list {
            let pid = client.get("pid").and_then(|v| v.as_u64())? as u32;
            if pid != wanted_pid {
                continue;
            }
            let address = client.get("address").and_then(|v| v.as_str())?.to_string();
            let at = client.get("at").and_then(|v| v.as_array())?;
            let size = client.get("size").and_then(|v| v.as_array())?;
            if at.len() < 2 || size.len() < 2 {
                continue;
            }
            let x = at[0].as_i64()? as i32;
            let y = at[1].as_i64()? as i32;
            let width = size[0].as_u64()? as u32;
            let height = size[1].as_u64()? as u32;
            if width == 0 || height == 0 {
                continue;
            }
            let floating = client.get("floating").and_then(|v| v.as_bool()).unwrap_or(false);
            return Some((address, WindowGeometry { x, y, width, height, floating }));
        }
    }
    None
}

/// Collect candidate host-window PIDs (KITTY_PID first, then parent chain up to PID 1).
#[cfg(all(target_os = "linux", not(test)))]
fn host_ancestor_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    if let Ok(pid) = std::env::var("KITTY_PID").and_then(|s| s.parse::<u32>().map_err(|_| std::env::VarError::NotPresent)) {
        pids.push(pid);
    }
    let mut current = std::process::id();
    for _ in 0..16 {
        let Ok(stat) = fs::read_to_string(format!("/proc/{current}/stat")) else { break };
        // Format: "pid (comm) state ppid ..." — split after the last ')' in case comm has spaces.
        let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else { break };
        let Some(ppid) = rest.split_whitespace().nth(1).and_then(|s| s.parse::<u32>().ok()) else { break };
        if ppid <= 1 || pids.contains(&ppid) {
            break;
        }
        pids.push(ppid);
        current = ppid;
    }
    pids
}

/// Query the current host window's Hyprland `(address, WindowGeometry)` if running under Hyprland.
#[cfg(all(target_os = "linux", not(test)))]
fn query_hyprland_window() -> Option<(String, WindowGeometry)> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let pids = host_ancestor_pids();
    if pids.is_empty() {
        return None;
    }
    let output = std::process::Command::new("hyprctl")
        .args(["clients", "-j"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    parse_hyprland_client(&text, &pids)
}

/// Capture the host terminal window's current position and size, if supported by the environment.
#[cfg(not(test))]
pub fn capture_geometry() -> Option<WindowGeometry> {
    #[cfg(target_os = "linux")]
    {
        query_hyprland_window().map(|(_, geom)| geom)
    }
    #[cfg(target_os = "macos")]
    {
        capture_macos_geometry()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Restore a saved host window geometry, if supported by the environment.
#[cfg(not(test))]
pub fn restore_geometry(geom: &WindowGeometry) {
    if geom.width == 0 || geom.height == 0 {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let Some((addr, current)) = query_hyprland_window() else { return };
        if geom.floating {
            let mut batch = String::new();
            if !current.floating {
                batch.push_str(&format!("dispatch setfloating address:{addr}; "));
            }
            batch.push_str(&format!(
                "dispatch resizewindowpixel exact {} {},address:{addr}; dispatch movewindowpixel exact {} {},address:{addr}",
                geom.width, geom.height, geom.x, geom.y
            ));
            let _ = std::process::Command::new("hyprctl")
                .args(["--batch", &batch])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        } else if !current.floating && (current.width != geom.width || current.height != geom.height) {
            let target = format!("exact {} {},address:{addr}", geom.width, geom.height);
            let _ = std::process::Command::new("hyprctl")
                .args(["dispatch", "resizewindowpixel", &target])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    #[cfg(target_os = "macos")]
    {
        restore_macos_geometry(geom);
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn macos_host_app() -> Option<&'static str> {
    if std::env::var_os("KITTY_PID").is_some() {
        Some("kitty")
    } else if std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some() || std::env::var_os("BAKEDIT_HOST").is_some() {
        Some("Ghostty")
    } else {
        None
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn capture_macos_geometry() -> Option<WindowGeometry> {
    let app = macos_host_app()?;
    let script = format!(
        r#"tell application "System Events" to tell process "{app}" to get {{position, size}} of front window"#
    );
    let output = std::process::Command::new("osascript")
        .args(["-e", &script])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let nums: Vec<i32> = text.split(',').filter_map(|s| s.trim().parse::<i32>().ok()).collect();
    if nums.len() >= 4 && nums[2] > 0 && nums[3] > 0 {
        Some(WindowGeometry {
            x: nums[0],
            y: nums[1],
            width: nums[2] as u32,
            height: nums[3] as u32,
            floating: true,
        })
    } else {
        None
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn restore_macos_geometry(geom: &WindowGeometry) {
    let Some(app) = macos_host_app() else { return };
    let script = format!(
        r#"tell application "System Events" to tell process "{app}"
    set position of front window to {{{}, {}}}
    set size of front window to {{{}, {}}}
end tell"#,
        geom.x, geom.y, geom.width, geom.height
    );
    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Spawn a background restore of zoom level and window geometry so startup is never blocked.
pub fn spawn_restore(state: WindowState) {
    #[cfg(not(test))]
    {
        if crate::window::running_in_test_pty() {
            return;
        }
        if state.zoom_offset == 0 && state.geometry.is_none() {
            return;
        }
        std::thread::spawn(move || {
            if let Some(geom) = &state.geometry {
                restore_geometry(geom);
            }
            if state.zoom_offset != 0 {
                restore_kitty_zoom(state.zoom_offset);
            }
        });
    }
    #[cfg(test)]
    let _ = state;
}

/// Capture the current host geometry and persist `state` under `state_dir` in the background.
pub fn spawn_capture_and_save(state_dir: PathBuf, #[cfg_attr(test, allow(unused_mut))] mut state: WindowState) {
    #[cfg(not(test))]
    {
        if crate::window::running_in_test_pty() {
            return;
        }
        std::thread::spawn(move || {
            if let Some(geom) = capture_geometry() {
                state.geometry = Some(geom);
            }
            let _ = save(&state_dir, &state);
        });
    }
    #[cfg(test)]
    let _ = (state_dir, state);
}

/// Synchronously capture the current host geometry and persist `state` (used on shutdown).
pub fn capture_and_save_sync(state_dir: &Path, state: &mut WindowState) {
    #[cfg(not(test))]
    {
        if crate::window::running_in_test_pty() {
            return;
        }
        if let Some(geom) = capture_geometry() {
            state.geometry = Some(geom);
        }
        let _ = save(state_dir, state);
    }
    #[cfg(test)]
    let _ = (state_dir, state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_clamping() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), WindowState::default());

        let state = WindowState {
            version: VERSION,
            zoom_offset: 3,
            geometry: Some(WindowGeometry { x: 100, y: 50, width: 1600, height: 900, floating: true }),
        };
        save(dir.path(), &state).unwrap();
        assert_eq!(load(dir.path()), state);

        // Corrupt and future-version files are safely ignored.
        fs::write(state_path(dir.path()), b"{not json").unwrap();
        assert_eq!(load(dir.path()), WindowState::default());
        fs::write(state_path(dir.path()), br#"{"version": 999, "zoom_offset": 5}"#).unwrap();
        assert_eq!(load(dir.path()), WindowState::default());
    }

    #[test]
    fn zoom_delta_accumulates_resets_and_clamps() {
        let mut z = 0;
        z = apply_zoom_delta(z, "+1");
        z = apply_zoom_delta(z, "+1");
        assert_eq!(z, 2);
        z = apply_zoom_delta(z, "-1");
        assert_eq!(z, 1);
        z = apply_zoom_delta(z, "0");
        assert_eq!(z, 0);
        for _ in 0..50 {
            z = apply_zoom_delta(z, "-1");
        }
        assert_eq!(z, MIN_ZOOM_OFFSET);
        for _ in 0..100 {
            z = apply_zoom_delta(z, "+1");
        }
        assert_eq!(z, MAX_ZOOM_OFFSET);
    }

    #[test]
    fn parses_hyprland_client_by_ancestor_pid() {
        let sample = r#"[
            {"address": "0x1", "pid": 999, "at": [0, 0], "size": [800, 600], "floating": false},
            {"address": "0x5632360c1ce0", "pid": 310185, "at": [12, 38], "size": [2728, 1102], "floating": true}
        ]"#;
        let (addr, geom) = parse_hyprland_client(sample, &[310242, 310185]).unwrap();
        assert_eq!(addr, "0x5632360c1ce0");
        assert_eq!(
            geom,
            WindowGeometry { x: 12, y: 38, width: 2728, height: 1102, floating: true }
        );
        assert!(parse_hyprland_client(sample, &[12345]).is_none());
    }
}
