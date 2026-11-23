//! Client for herdr's socket API and native pane navigation.

mod client;
#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use client::{
    is_mutating, Client, HerdrError, ServerInfo, DEFAULT_TIMEOUT, MAX_TIMEOUT, SOCKET_ENV_VAR,
};
