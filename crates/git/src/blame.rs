//! Per-line git blame for the editor.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::error::Result;
use crate::paths::literal_pathspec;
use crate::repo::Repo;

/// Metadata for a single blamed line (shared across all lines from the same commit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameInfo {
    /// Full 40-character commit hash (all zeroes for uncommitted changes).
    pub sha: String,
    /// Abbreviated 7-character hash (empty for uncommitted changes).
    pub short: String,
    /// Author name (e.g. `"Nathan Baker"` or `"You"` for uncommitted lines).
    pub author: String,
    /// Author timestamp in Unix seconds.
    pub timestamp: i64,
    /// First line of the commit message.
    pub summary: String,
    /// True when the line has not been committed yet (`00000000...`).
    pub uncommitted: bool,
}

/// Blame results for every line of a file, plus the most recent commit that touched it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBlame {
    /// One entry per 0-based line of the file.
    pub lines: Vec<Arc<BlameInfo>>,
    /// The newest committed [`BlameInfo`] in the file, for the file header.
    pub last_commit: Option<Arc<BlameInfo>>,
}

impl FileBlame {
    /// Blame info for 0-based `line`.
    pub fn line(&self, line: usize) -> Option<&BlameInfo> {
        self.lines.get(line).map(Arc::as_ref)
    }
}

/// Format a Unix timestamp relative to `now` in compact form (`just now`, `5m ago`, `3h ago`,
/// `2d ago`, `3w ago`, `4mo ago`, `2y ago`).
pub fn format_relative_time(timestamp: i64, now: i64) -> String {
    let diff = now.saturating_sub(timestamp);
    if diff < 60 {
        return "just now".to_string();
    }
    let mins = diff / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 14 {
        return format!("{days}d ago");
    }
    let weeks = days / 7;
    if weeks < 8 {
        return format!("{weeks}w ago");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo ago");
    }
    let years = days / 365;
    format!("{years}y ago")
}

/// Parse `git blame --porcelain` output into a [`FileBlame`].
pub fn parse_blame_porcelain(output: &[u8]) -> FileBlame {
    let text = String::from_utf8_lossy(output);
    let mut commits: HashMap<String, Arc<BlameInfo>> = HashMap::new();
    let mut lines: Vec<Arc<BlameInfo>> = Vec::new();
    let mut last_commit: Option<Arc<BlameInfo>> = None;

    let mut iter = text.lines().peekable();
    while let Some(header) = iter.next() {
        let mut parts = header.split_whitespace();
        let Some(sha) = parts.next() else { continue };
        if sha.len() < 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let uncommitted = sha.bytes().all(|b| b == b'0');
        let mut author: Option<String> = None;
        let mut timestamp: Option<i64> = None;
        let mut summary: Option<String> = None;

        // Read header fields until the content line (which starts with '\t').
        for line in iter.by_ref() {
            if line.starts_with('\t') {
                break;
            }
            if let Some(rest) = line.strip_prefix("author ") {
                author = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("author-time ") {
                timestamp = rest.trim().parse().ok();
            } else if let Some(rest) = line.strip_prefix("summary ") {
                summary = Some(rest.to_string());
            }
        }

        let info = if let Some(existing) = commits.get(sha) {
            Arc::clone(existing)
        } else {
            let info = Arc::new(BlameInfo {
                sha: sha.to_string(),
                short: if uncommitted { String::new() } else { sha[..7].to_string() },
                author: if uncommitted {
                    "You".to_string()
                } else {
                    author.unwrap_or_else(|| "Unknown".to_string())
                },
                timestamp: timestamp.unwrap_or(0),
                summary: if uncommitted {
                    "Uncommitted changes".to_string()
                } else {
                    summary.unwrap_or_default()
                },
                uncommitted,
            });
            commits.insert(sha.to_string(), Arc::clone(&info));
            info
        };

        if !info.uncommitted {
            let replace = match &last_commit {
                None => true,
                Some(prev) => info.timestamp > prev.timestamp,
            };
            if replace {
                last_commit = Some(Arc::clone(&info));
            }
        }
        lines.push(info);
    }

    FileBlame { lines, last_commit }
}

impl Repo {
    /// Run `git blame --porcelain` on `path` and return per-line blame info plus the newest
    /// commit that touched the file.
    pub fn blame(&self, path: &Path) -> Result<FileBlame> {
        let relative = self.relative_path(path)?;
        let output = self
            .git()
            .args(["blame", "--porcelain", "--"])
            .arg(literal_pathspec(&relative))
            .run()?;
        Ok(parse_blame_porcelain(&output.stdout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_and_finds_last_commit() {
        let raw = "\
1111111111111111111111111111111111111111 1 1 2
author Alice
author-mail <alice@example.com>
author-time 1700000000
author-tz +0000
committer Alice
committer-mail <alice@example.com>
committer-time 1700000000
committer-tz +0000
summary initial commit
filename a.rs
\tfn main() {}
1111111111111111111111111111111111111111 2 2
\t// line 2
2222222222222222222222222222222222222222 3 3 1
author Nathan Baker
author-mail <n@example.com>
author-time 1700003600
author-tz +0000
summary feat: add blame
filename a.rs
\tprintln!(\"hi\");
0000000000000000000000000000000000000000 4 4 1
author Not Committed Yet
author-time 1700007200
summary Version of a.rs from the index
filename a.rs
\t// wip
";
        let blame = parse_blame_porcelain(raw.as_bytes());
        assert_eq!(blame.lines.len(), 4);
        assert_eq!(blame.line(0).unwrap().author, "Alice");
        assert_eq!(blame.line(1).unwrap().author, "Alice");
        assert_eq!(blame.line(2).unwrap().author, "Nathan Baker");
        assert_eq!(blame.line(2).unwrap().summary, "feat: add blame");
        assert!(blame.line(3).unwrap().uncommitted);
        assert_eq!(blame.line(3).unwrap().author, "You");
        let last = blame.last_commit.as_ref().unwrap();
        assert_eq!(last.author, "Nathan Baker");
        assert_eq!(last.short, "2222222");
    }

    #[test]
    fn formats_relative_times() {
        assert_eq!(format_relative_time(1000, 1020), "just now");
        assert_eq!(format_relative_time(1000, 1300), "5m ago");
        assert_eq!(format_relative_time(1000, 1000 + 7200), "2h ago");
        assert_eq!(format_relative_time(1000, 1000 + 86400 * 3), "3d ago");
    }
}
