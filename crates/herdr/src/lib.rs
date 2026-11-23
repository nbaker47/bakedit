//! Client for herdr's socket API and native pane navigation.

mod client;
mod collapse;
mod layout;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod nav;
mod state;
mod types;

pub use client::{
    is_mutating, Client, HerdrError, ServerInfo, DEFAULT_TIMEOUT, MAX_TIMEOUT, SOCKET_ENV_VAR,
};
pub use collapse::{collapse, collapse_pane, CollapseMode, CollapseOutcome};
pub use nav::{navigate, NavOutcome};
pub use types::{Direction, PaneInfo, ParseDirectionError, Rect, ScrollInfo, SplitDirection};
