//! The plain data this crate hands out.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A direction on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Towards the left edge.
    Left,
    /// Towards the right edge.
    Right,
    /// Towards the top.
    Up,
    /// Towards the bottom.
    Down,
}

impl Direction {
    /// All four directions.
    pub const ALL: [Direction; 4] = [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ];

    /// The name herdr uses: `left`, `right`, `up` or `down`.
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }

    /// Whether this is `Left` or `Right`.
    pub fn is_horizontal(self) -> bool {
        matches!(self, Direction::Left | Direction::Right)
    }

    /// Whether this points to the far end of its axis: `Right` or `Down`.
    pub fn is_forward(self) -> bool {
        matches!(self, Direction::Right | Direction::Down)
    }

    /// The direction that undoes this one.
    pub fn opposite(self) -> Direction {
        match self {
            Direction::Left => Direction::Right,
            Direction::Right => Direction::Left,
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A string that is not `left`, `right`, `up` or `down`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a direction (expected left, right, up or down)")]
pub struct ParseDirectionError(pub String);

impl FromStr for Direction {
    type Err = ParseDirectionError;

    /// Parse `left`, `right`, `up` or `down`, in any letter case.
    fn from_str(text: &str) -> Result<Direction, ParseDirectionError> {
        Direction::ALL
            .into_iter()
            .find(|direction| direction.as_str().eq_ignore_ascii_case(text))
            .ok_or_else(|| ParseDirectionError(text.to_string()))
    }
}

/// The two ways herdr divides a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    /// Side by side: the second half is to the right of the first.
    Right,
    /// Stacked: the second half is below the first.
    Down,
}

impl SplitDirection {
    /// The name herdr uses: `right` or `down`.
    pub fn as_str(self) -> &'static str {
        match self {
            SplitDirection::Right => "right",
            SplitDirection::Down => "down",
        }
    }

    /// The split a move in `direction` runs along: `Right` for left and right, `Down` for up
    /// and down.
    pub fn along(direction: Direction) -> SplitDirection {
        if direction.is_horizontal() {
            SplitDirection::Right
        } else {
            SplitDirection::Down
        }
    }
}

impl fmt::Display for SplitDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How far a pane is scrolled back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub struct ScrollInfo {
    /// Lines between the bottom of the view and the bottom of the output; 0 is the live end.
    #[serde(default)]
    pub offset_from_bottom: u64,
    /// How far back the view can go. 0 means the pane has no scrollback, which is how a
    /// full-screen program on the alternate screen shows up.
    #[serde(default)]
    pub max_offset_from_bottom: u64,
    /// Rows of the pane's terminal.
    #[serde(default = "default_viewport_rows")]
    pub viewport_rows: u64,
}

fn default_viewport_rows() -> u64 {
    24
}

impl Default for ScrollInfo {
    /// What the scripts assume when herdr reports no scroll state: no scrollback, 24 rows.
    fn default() -> ScrollInfo {
        ScrollInfo {
            offset_from_bottom: 0,
            max_offset_from_bottom: 0,
            viewport_rows: default_viewport_rows(),
        }
    }
}

/// One herdr pane.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneInfo {
    /// The pane's id, such as `w2:p1`. It stays the same while the pane stays in its
    /// workspace.
    pub pane_id: String,
    /// The tab the pane is in, such as `w2:t1`.
    pub tab_id: String,
    /// The workspace the pane is in, such as `w2`.
    pub workspace_id: String,
    /// Whether this is the pane herdr sends keys to.
    #[serde(default)]
    pub focused: bool,
    /// The working directory of the pane's shell.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// The working directory of the program running in the foreground of the pane, which
    /// differs from `cwd` when that program was started elsewhere or changed directory.
    #[serde(default)]
    pub foreground_cwd: Option<PathBuf>,
    /// The name the user gave the pane, if any.
    #[serde(default)]
    pub label: Option<String>,
    /// The title the pane's program set, if any.
    #[serde(default)]
    pub terminal_title: Option<String>,
    /// The coding agent herdr detected in the pane (`claude`, `codex`, ...), if any.
    #[serde(default)]
    pub agent: Option<String>,
    /// The pane's scroll state, when herdr reports it.
    #[serde(default)]
    pub scroll: Option<ScrollInfo>,
}

impl PaneInfo {
    /// The directory a relative path printed in this pane most likely refers to: the
    /// foreground program's directory when herdr knows it, else the shell's. An empty path
    /// counts as not known.
    pub fn working_dir(&self) -> Option<&Path> {
        known_dir(&self.foreground_cwd).or(known_dir(&self.cwd))
    }
}

/// `dir` unless herdr left it out or sent an empty string.
fn known_dir(dir: &Option<PathBuf>) -> Option<&Path> {
    dir.as_deref().filter(|dir| !dir.as_os_str().is_empty())
}

/// A rectangle of terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
pub struct Rect {
    /// The leftmost column.
    pub x: u16,
    /// The topmost row.
    pub y: u16,
    /// Columns covered.
    pub width: u16,
    /// Rows covered.
    pub height: u16,
}

impl Rect {
    /// A rectangle from its top-left corner and size.
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether cell `(col, row)` lies inside. An empty rectangle contains nothing.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        let inside = |value: u16, start: u16, length: u16| value >= start && value - start < length;
        inside(col, self.x, self.width) && inside(row, self.y, self.height)
    }

    /// The same rectangle moved right by `cols` and down by `rows`, stopping at the largest
    /// coordinate instead of wrapping.
    pub fn translated(&self, cols: u16, rows: u16) -> Rect {
        Rect {
            x: self.x.saturating_add(cols),
            y: self.y.saturating_add(rows),
            ..*self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn directions_have_herdr_names() {
        assert_eq!(Direction::Left.to_string(), "left");
        assert_eq!(
            serde_json::to_value(Direction::Down).unwrap(),
            json!("down")
        );
        assert_eq!(
            serde_json::from_value::<Direction>(json!("up")).unwrap(),
            Direction::Up
        );
        assert_eq!(
            serde_json::to_value(SplitDirection::Right).unwrap(),
            json!("right")
        );
        assert_eq!(SplitDirection::Down.to_string(), "down");
    }

    #[test]
    fn directions_parse_from_text() {
        assert_eq!("left".parse::<Direction>(), Ok(Direction::Left));
        assert_eq!("Right".parse::<Direction>(), Ok(Direction::Right));
        assert_eq!("UP".parse::<Direction>(), Ok(Direction::Up));
        assert_eq!("down".parse::<Direction>(), Ok(Direction::Down));
        let err = "sideways".parse::<Direction>().unwrap_err();
        assert_eq!(err, ParseDirectionError("sideways".into()));
        assert!(err.to_string().contains("sideways"));
        assert!("".parse::<Direction>().is_err());
    }

    #[test]
    fn direction_helpers() {
        for direction in Direction::ALL {
            assert_eq!(direction.opposite().opposite(), direction);
            assert_ne!(direction.opposite(), direction);
            assert_eq!(
                direction.is_horizontal(),
                direction.opposite().is_horizontal()
            );
            assert_ne!(direction.is_forward(), direction.opposite().is_forward());
        }
        assert!(Direction::Left.is_horizontal() && Direction::Right.is_horizontal());
        assert!(!Direction::Up.is_horizontal() && !Direction::Down.is_horizontal());
        assert!(Direction::Right.is_forward() && Direction::Down.is_forward());
        assert_eq!(
            SplitDirection::along(Direction::Left),
            SplitDirection::Right
        );
        assert_eq!(
            SplitDirection::along(Direction::Right),
            SplitDirection::Right
        );
        assert_eq!(SplitDirection::along(Direction::Up), SplitDirection::Down);
        assert_eq!(SplitDirection::along(Direction::Down), SplitDirection::Down);
    }

    #[test]
    fn pane_info_reads_a_real_reply_and_ignores_what_it_does_not_know() {
        let pane: PaneInfo = serde_json::from_value(json!({
            "pane_id": "w1:p2",
            "terminal_id": "term_660e99c6047a32",
            "workspace_id": "w1",
            "tab_id": "w1:t1",
            "focused": true,
            "cwd": "/work/b",
            "foreground_cwd": "/work/b/sub",
            "agent": "claude",
            "agent_status": "idle",
            "scroll": {"offset_from_bottom": 3, "max_offset_from_bottom": 183, "viewport_rows": 38},
            "revision": 0
        }))
        .unwrap();
        assert_eq!(pane.pane_id, "w1:p2");
        assert_eq!(pane.tab_id, "w1:t1");
        assert_eq!(pane.workspace_id, "w1");
        assert!(pane.focused);
        assert_eq!(pane.cwd.as_deref(), Some(Path::new("/work/b")));
        assert_eq!(pane.working_dir(), Some(Path::new("/work/b/sub")));
        assert_eq!(pane.agent.as_deref(), Some("claude"));
        assert_eq!(
            pane.scroll,
            Some(ScrollInfo {
                offset_from_bottom: 3,
                max_offset_from_bottom: 183,
                viewport_rows: 38
            })
        );
        assert_eq!(pane.label, None);
    }

    #[test]
    fn pane_info_needs_only_its_ids() {
        let pane: PaneInfo = serde_json::from_value(
            json!({"pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1"}),
        )
        .unwrap();
        assert!(!pane.focused);
        assert_eq!(pane.working_dir(), None);
        assert_eq!(pane.scroll, None);
        assert!(serde_json::from_value::<PaneInfo>(json!({"pane_id": "w1:p1"})).is_err());
    }

    #[test]
    fn working_dir_falls_back_to_the_shell_directory() {
        let pane: PaneInfo = serde_json::from_value(
            json!({"pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "cwd": "/work/a"}),
        )
        .unwrap();
        assert_eq!(pane.working_dir(), Some(Path::new("/work/a")));
    }

    #[test]
    fn an_empty_directory_is_no_directory() {
        let pane: PaneInfo = serde_json::from_value(json!({
            "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "cwd": "/work/a", "foreground_cwd": "",
        }))
        .unwrap();
        assert_eq!(pane.working_dir(), Some(Path::new("/work/a")));
        let pane: PaneInfo = serde_json::from_value(json!({
            "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "cwd": "",
        }))
        .unwrap();
        assert_eq!(pane.working_dir(), None);
    }

    #[test]
    fn scroll_info_defaults_match_the_scripts() {
        assert_eq!(
            ScrollInfo::default(),
            ScrollInfo {
                offset_from_bottom: 0,
                max_offset_from_bottom: 0,
                viewport_rows: 24
            }
        );
        assert_eq!(
            serde_json::from_value::<ScrollInfo>(json!({})).unwrap(),
            ScrollInfo::default()
        );
    }

    #[test]
    fn rect_contains_its_cells_and_nothing_else() {
        let rect = Rect::new(10, 5, 4, 2);
        assert!(rect.contains(10, 5));
        assert!(rect.contains(13, 6));
        assert!(!rect.contains(14, 5));
        assert!(!rect.contains(10, 7));
        assert!(!rect.contains(9, 5));
        assert!(!rect.contains(10, 4));
        assert!(!Rect::new(3, 3, 0, 5).contains(3, 3));
        assert!(Rect::new(65535, 65535, 1, 1).contains(65535, 65535));
    }

    #[test]
    fn rect_translation_saturates() {
        assert_eq!(
            Rect::new(1, 2, 3, 4).translated(26, 1),
            Rect::new(27, 3, 3, 4)
        );
        assert_eq!(Rect::new(65000, 2, 3, 4).translated(1000, 0).x, 65535);
    }
}
