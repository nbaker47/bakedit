//! Errors from starting a terminal.

use std::io;

/// Why a terminal could not be started.
#[derive(Debug, thiserror::Error)]
pub enum TermError {
    /// The system would not hand out a pseudo-terminal.
    #[error("could not open a pseudo-terminal: {0}")]
    Pty(#[source] io::Error),
    /// The program could not be run. `source.kind()` is [`io::ErrorKind::NotFound`] when
    /// there is no such program, which is the case worth falling back to a shell for.
    #[error("could not start {program:?}: {source}")]
    Spawn {
        /// The program as it was asked for.
        program: String,
        /// What the system said.
        #[source]
        source: io::Error,
    },
    /// Setting up the reader for the pty failed.
    #[error("could not set up terminal I/O: {0}")]
    Io(#[from] io::Error),
    /// A command line could not be split into a program and its arguments.
    #[error("invalid command line: {0}")]
    CommandLine(String),
}

impl TermError {
    /// Whether this is "there is no such program": the configured command is not installed
    /// (or its path is wrong), as opposed to something being wrong with the system.
    pub fn is_program_missing(&self) -> bool {
        matches!(self, TermError::Spawn { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }
}

/// The result type of this crate.
pub type Result<T> = std::result::Result<T, TermError>;
