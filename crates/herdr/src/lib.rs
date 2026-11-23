//! Client for herdr's socket API and native pane navigation.

mod client;
mod collapse;
mod layout;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod move_pane;
mod nav;
mod query;
mod root_split;
mod scroll;
mod state;
mod types;

pub use client::{
    is_mutating, Client, HerdrError, ServerInfo, DEFAULT_TIMEOUT, MAX_TIMEOUT, SOCKET_ENV_VAR,
};
pub use collapse::{collapse, collapse_pane, CollapseMode, CollapseOutcome};
pub use move_pane::{move_pane, MoveOutcome};
pub use nav::{navigate, NavOutcome};
pub use query::{
    focused_pane, pane_at, pane_info, pane_layout, PaneAt, PaneLayout, PaneRect, ScreenSize,
};
pub use root_split::{root_split, root_split_with_share, DEFAULT_ROOT_SPLIT_SHARE};
pub use scroll::{scroll, ScrollOutcome};
pub use types::{Direction, PaneInfo, ParseDirectionError, Rect, ScrollInfo, SplitDirection};
