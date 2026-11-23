//! Client for herdr's socket API and native pane navigation.

mod client;
mod layout;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod state;
mod types;

pub use client::{
    is_mutating, Client, HerdrError, ServerInfo, DEFAULT_TIMEOUT, MAX_TIMEOUT, SOCKET_ENV_VAR,
};
pub use types::{Direction, PaneInfo, ParseDirectionError, Rect, ScrollInfo, SplitDirection};
