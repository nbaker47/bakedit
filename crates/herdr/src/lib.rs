//! A client for herdr's socket API, and native versions of the scripts that drive herdr's
//! panes.
//!
//! [herdr](https://herdr.dev) is the terminal workspace manager that runs in the IDE's
//! terminal panel. It exposes a JSON API on a unix socket. [`Client`] speaks that API, and the
//! functions built on it do what the user's `herdr-*` scripts did from a key chord, without
//! the chord, the shell and the Python start-up in between:
//!
//! | Function | Script | What it does |
//! |---|---|---|
//! | [`navigate`] | `herdr-nav` | focus the neighbouring pane; at an edge hop to the next tab or workspace; then auto-expand |
//! | [`move_pane`] | `herdr-move` | swap with the neighbour, or turn the parent split |
//! | [`collapse`] | `herdr-collapse` | collapse, expand or auto-expand a pane |
//! | [`collapse_pane`] | the auto-expand plugin | the same for the pane a focus event named |
//! | [`scroll`] | `herdr-scroll` | scroll the focused pane, or send it wheel events |
//! | [`root_split`] | `herdr-root-split` | add a pane spanning the whole tab |
//! | [`zoom_toggle`], [`previous_workspace`], [`next_workspace`], [`focus_workspace_number`] | herdr's own keys | zoom and workspace switching |
//! | [`focused_pane`], [`pane_at`] | | which pane is focused, which pane is under a screen cell |
//!
//! Every function takes the client it should use. Nothing reads the environment except
//! [`Client::from_env_or_default`] and the mutation guard, which notes once per process where
//! the user's live session is so that a client cannot change it by accident (see
//! [`Client::allow_live_mutations`]).
//!
//! Collapsing remembers split sizes in a state file shared with the `herdr-collapse` script,
//! in that script's format, so the two interoperate; see [`Client::with_state_file`].
//!
//! The functions are plain blocking calls, each made of a handful of socket round trips, and
//! each round trip is bounded by the client's timeout. They hold no locks and keep no state
//! between calls, but two of them changing the same tab at the same time interleave like two
//! copies of a script would: run them from one thread at a time.

mod client;
mod collapse;
pub mod launch;
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
mod workspace;

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
pub use workspace::{
    focus_workspace_number, next_workspace, previous_workspace, zoom_toggle, WorkspaceOutcome,
    ZoomOutcome,
};
