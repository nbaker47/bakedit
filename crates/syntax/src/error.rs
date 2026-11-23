//! Errors from highlighting.

/// Why a highlighting call returned nothing.
///
/// Broken source is never an error: tree-sitter recovers from syntax errors and the rest of
/// the file is still highlighted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyntaxError {
    /// The caller's cancel flag was set before the work finished.
    #[error("highlighting was cancelled")]
    Cancelled,
    /// A bundled grammar or one of its queries failed to load. This is a bug in the crate,
    /// not in the text; the test suite loads every grammar.
    #[error("the {language} grammar could not be loaded: {message}")]
    Grammar {
        /// The grammar's name, such as `rust`.
        language: &'static str,
        /// What tree-sitter reported.
        message: String,
    },
    /// The text is longer than tree-sitter can address (4 GiB).
    #[error("the text is too large to parse ({bytes} bytes)")]
    TooLarge {
        /// The length of the text.
        bytes: usize,
    },
}
