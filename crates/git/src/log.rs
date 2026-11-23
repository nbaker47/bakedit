//! Commit history for the history list.

use std::path::Path;

use crate::error::Result;
use crate::paths::literal_pathspec;
use crate::repo::Repo;

/// The fields asked of `git log`, NUL separated. With `-z` every commit ends in a NUL as
/// well, so the output is one flat list of fields.
const LOG_FORMAT: &str = "--format=%H%x00%h%x00%aN%x00%at%x00%ar%x00%s";

/// How many fields [`LOG_FORMAT`] asks for.
const LOG_FIELDS: usize = 6;

/// One commit in a history list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// The full hash.
    pub sha: String,
    /// The abbreviated hash, as long as git needs it to be unambiguous.
    pub short: String,
    /// The author's name (after `.mailmap`).
    pub author: String,
    /// When the commit was authored, in seconds since the Unix epoch.
    pub time: i64,
    /// The same moment relative to now, in git's words: `3 hours ago`.
    pub relative_time: String,
    /// The first line of the commit message.
    pub subject: String,
}

/// Reads the output of `git log -z` with [`LOG_FORMAT`]. An incomplete last record is
/// dropped.
fn parse_log(output: &[u8]) -> Vec<Commit> {
    let fields: Vec<&[u8]> = output.split(|&byte| byte == 0).collect();
    fields
        .chunks_exact(LOG_FIELDS)
        .map(|record| {
            let text = |index: usize| String::from_utf8_lossy(record[index]).into_owned();
            Commit {
                sha: text(0),
                short: text(1),
                author: text(2),
                time: text(3).parse().unwrap_or(0),
                relative_time: text(4),
                subject: text(5),
            }
        })
        .collect()
}

impl Repo {
    /// The newest `limit` commits reachable from `HEAD`, newest first. With `path`, only
    /// the commits that changed that file or directory; a file's history is followed
    /// through renames.
    ///
    /// Empty before the first commit and when `limit` is 0.
    pub fn log(&self, limit: usize, path: Option<&Path>) -> Result<Vec<Commit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut command =
            self.git()
                .args(["log", "-z", &format!("--max-count={limit}"), LOG_FORMAT]);
        if let Some(path) = path {
            let relative = self.relative_path(path)?;
            // The top of the work tree is every path: no filter needed.
            if !relative.as_os_str().is_empty() {
                command = command
                    .arg("--follow")
                    .arg("--")
                    .arg(literal_pathspec(&relative));
            }
        }
        match command.run() {
            Ok(output) => Ok(parse_log(&output.stdout)),
            // `git log` fails on a branch without commits; that is an empty history.
            Err(error) => match self.head_sha()? {
                None => Ok(Vec::new()),
                Some(_) => Err(error),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nul_terminated(fields: &[&str]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for field in fields {
            bytes.extend_from_slice(field.as_bytes());
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn parses_commits_in_order() {
        let output = nul_terminated(&[
            "8cbc8e3948a4d8437af274ed8924f9a77582e614",
            "8cbc8e3",
            "Ada Lovelace",
            "1795406241",
            "2 hours ago",
            "fix: handle the empty case",
            "05c16969050e1b3ed1da06fa5e46f34bac38a6f9",
            "05c1696",
            "Søren Kierkegaard",
            "1795400000",
            "4 hours ago",
            "",
        ]);
        let commits = parse_log(&output);
        assert_eq!(
            commits,
            [
                Commit {
                    sha: "8cbc8e3948a4d8437af274ed8924f9a77582e614".to_string(),
                    short: "8cbc8e3".to_string(),
                    author: "Ada Lovelace".to_string(),
                    time: 1795406241,
                    relative_time: "2 hours ago".to_string(),
                    subject: "fix: handle the empty case".to_string(),
                },
                Commit {
                    sha: "05c16969050e1b3ed1da06fa5e46f34bac38a6f9".to_string(),
                    short: "05c1696".to_string(),
                    author: "Søren Kierkegaard".to_string(),
                    time: 1795400000,
                    relative_time: "4 hours ago".to_string(),
                    subject: String::new(),
                },
            ]
        );
    }

    #[test]
    fn empty_and_truncated_output() {
        assert!(parse_log(b"").is_empty());
        let mut output = nul_terminated(&["sha", "short", "author", "12", "now", "subject"]);
        output.extend_from_slice(b"second-sha\0cut");
        let commits = parse_log(&output);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "subject");
    }

    #[test]
    fn subjects_keep_their_punctuation_and_bad_times_become_zero() {
        let output = nul_terminated(&[
            "sha",
            "short",
            "a\u{1f}b",
            "soon",
            "in the future",
            "feat(x): tabs\tand | pipes \u{1f} and separators",
        ]);
        let commits = parse_log(&output);
        assert_eq!(commits[0].time, 0);
        assert_eq!(commits[0].author, "a\u{1f}b");
        assert_eq!(
            commits[0].subject,
            "feat(x): tabs\tand | pipes \u{1f} and separators"
        );
    }
    #[test]
    fn mangled_output_never_panics() {
        let sample = nul_terminated(&[
            "8cbc8e3948a4d8437af274ed8924f9a77582e614",
            "8cbc8e3",
            "Ada Lovelace",
            "1795406241",
            "2 hours ago",
            "fix: handle the empty case",
            "05c16969050e1b3ed1da06fa5e46f34bac38a6f9",
            "05c1696",
            "S\u{f8}ren",
            "-5",
            "4 hours ago",
            "",
        ]);
        let mut random = crate::testing::Lcg(5);
        for _ in 0..20_000 {
            let mangled = random.mangle(&sample);
            let fields = mangled.iter().filter(|&&byte| byte == 0).count() + 1;
            assert!(parse_log(&mangled).len() * LOG_FIELDS <= fields);
        }
    }
}
