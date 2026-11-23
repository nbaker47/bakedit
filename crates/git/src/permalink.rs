//! Links to a file's lines on the web site that hosts the repository.

use std::path::Path;

use crate::error::{GitError, Result};
use crate::paths::path_bytes;
use crate::repo::Repo;

/// The kind of hosting site, which decides how its links are spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostKind {
    /// github.com, GitHub Enterprise, and every host that is not recognised: GitHub's
    /// link format is the one most other sites understand.
    GitHub,
    /// gitlab.com and self-hosted GitLab (a host name that contains `gitlab`).
    GitLab,
    /// bitbucket.org.
    Bitbucket,
    /// codeberg.org and hosts whose name contains `gitea` or `forgejo`.
    Gitea,
}

/// A remote URL reduced to what a web link needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    /// `https`, or `http` when the remote itself is fetched over plain http.
    pub scheme: String,
    /// The host, in lower case. A port is kept only for http and https remotes; the port
    /// of an ssh remote is not the web site's.
    pub host: String,
    /// The repository's path on the host, usually `owner/repo` (GitLab subgroups make it
    /// longer), without the `.git` suffix and without slashes at either end.
    pub path: String,
}

impl RemoteUrl {
    /// The repository's page: `https://github.com/owner/repo`.
    pub fn web_url(&self) -> String {
        format!("{}://{}/{}", self.scheme, self.host, self.path)
    }

    /// Which site's link format the host uses.
    pub fn host_kind(&self) -> HostKind {
        let host = self.host.split(':').next().unwrap_or_default();
        if host == "bitbucket.org" {
            HostKind::Bitbucket
        } else if host.contains("gitlab") {
            HostKind::GitLab
        } else if host == "codeberg.org" || host.contains("gitea") || host.contains("forgejo") {
            HostKind::Gitea
        } else {
            HostKind::GitHub
        }
    }
}

/// Reads a git remote URL in any of its common spellings:
///
/// - `git@host:owner/repo.git` (also without the user, and without `.git`)
/// - `https://host/owner/repo.git`, `http://...`, with or without a user, password or
///   port
/// - `ssh://git@host:port/owner/repo.git`, `git://host/owner/repo.git`,
///   `git+ssh://...`
///
/// Returns `None` for anything that does not name a repository on a host: local paths,
/// `file://` URLs, Windows drive paths, and URLs without a host or a path.
pub fn parse_remote_url(url: &str) -> Option<RemoteUrl> {
    let url = url.trim();
    let (scheme, host, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            let scheme = scheme.to_ascii_lowercase();
            let (authority, path) = rest.split_once('/')?;
            // Whatever comes before the last `@` is a user name and maybe a password.
            let host_and_port = authority.rsplit('@').next()?;
            match scheme.as_str() {
                "http" | "https" => (scheme, host_and_port.to_string(), path),
                "ssh" | "git" | "git+ssh" | "ssh+git" => (
                    "https".to_string(),
                    without_port(host_and_port).to_string(),
                    path,
                ),
                _ => return None,
            }
        }
        None => {
            // user@host:path, which git only reads this way when no slash comes before
            // the colon. A single letter before the colon is a Windows drive.
            let (user_and_host, path) = url.split_once(':')?;
            if user_and_host.contains('/') || user_and_host.len() < 2 {
                return None;
            }
            let host = user_and_host.rsplit('@').next()?;
            ("https".to_string(), host.to_string(), path)
        }
    };
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    let host = host.to_ascii_lowercase();
    let host_is_sane = !host.is_empty()
        && !host.starts_with(':')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '[' | ']'));
    if !host_is_sane || path.is_empty() {
        return None;
    }
    Some(RemoteUrl {
        scheme,
        host,
        path: path.to_string(),
    })
}

/// `host:port` without the port. An IPv6 address in brackets keeps its colons.
fn without_port(host_and_port: &str) -> &str {
    match host_and_port.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => host,
        _ => host_and_port,
    }
}

/// The link to lines `first_line` to `last_line` (1-based, inclusive) of `path` as it is
/// in commit `sha`, on the site that hosts `remote`:
///
/// - GitHub and unknown hosts: `https://host/owner/repo/blob/<sha>/<path>#L10-L20`
/// - GitLab: `https://host/owner/repo/-/blob/<sha>/<path>#L10-20`
/// - Bitbucket: `https://bitbucket.org/owner/repo/src/<sha>/<path>#lines-10:20`
/// - Gitea, Forgejo, Codeberg: `https://host/owner/repo/src/commit/<sha>/<path>#L10-L20`
///
/// One line is written as a single number (`#L10`); the two numbers may come in either
/// order, and 0 counts as line 1. Every segment of `path` (relative to the top of the
/// work tree) is percent-encoded. On GitHub and GitLab a Markdown file gets `?plain=1`,
/// without which those sites show the rendered page, where lines cannot be marked.
pub fn build_permalink(
    remote: &RemoteUrl,
    sha: &str,
    path: &Path,
    first_line: u32,
    last_line: u32,
) -> String {
    let first = first_line.min(last_line).max(1);
    let last = first_line.max(last_line).max(1);
    let single = first == last;
    let kind = remote.host_kind();
    let lines = match kind {
        HostKind::GitHub | HostKind::Gitea if single => format!("L{first}"),
        HostKind::GitHub | HostKind::Gitea => format!("L{first}-L{last}"),
        HostKind::GitLab if single => format!("L{first}"),
        HostKind::GitLab => format!("L{first}-{last}"),
        HostKind::Bitbucket if single => format!("lines-{first}"),
        HostKind::Bitbucket => format!("lines-{first}:{last}"),
    };
    let tree = match kind {
        HostKind::GitHub => "blob",
        HostKind::GitLab => "-/blob",
        HostKind::Bitbucket => "src",
        HostKind::Gitea => "src/commit",
    };
    let rendered_unless_asked = matches!(kind, HostKind::GitHub | HostKind::GitLab)
        && path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        });
    let query = if rendered_unless_asked {
        "?plain=1"
    } else {
        ""
    };
    format!(
        "{}/{tree}/{sha}/{}{query}#{lines}",
        remote.web_url(),
        encode_path(path)
    )
}

/// `path` with every byte that is not a letter, a digit or one of `-._~` percent-encoded.
/// The slashes between segments stay.
fn encode_path(path: &Path) -> String {
    let mut encoded = String::new();
    for &byte in path_bytes(path) {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                encoded.push(char::from(byte))
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

impl Repo {
    /// A link to lines `first_line` to `last_line` (1-based, inclusive) of `path` at the
    /// commit that is checked out, on the web site of the repository's remote. See
    /// [`build_permalink`] for the formats.
    ///
    /// The remote is the one the current branch tracks, else `origin`, else the only
    /// remote there is. The link names the full hash of `HEAD`, so it keeps pointing at
    /// the same lines; it only works for others once that commit has been pushed, and
    /// the line numbers are those of the committed file, which unsaved or uncommitted
    /// edits may have moved.
    ///
    /// Errors: [`GitError::NoCommits`] before the first commit, [`GitError::NoRemote`]
    /// when no remote can be chosen, [`GitError::UnsupportedRemoteUrl`] when the remote
    /// is a local path, and [`GitError::PathOutsideRepo`].
    pub fn permalink(
        &self,
        path: impl AsRef<Path>,
        first_line: u32,
        last_line: u32,
    ) -> Result<String> {
        let relative = self.relative_path(path)?;
        let sha = self.head_sha()?.ok_or(GitError::NoCommits)?;
        let url = self.remote_url(&self.permalink_remote()?)?;
        let remote = parse_remote_url(&url).ok_or(GitError::UnsupportedRemoteUrl(url))?;
        Ok(build_permalink(
            &remote, &sha, &relative, first_line, last_line,
        ))
    }

    /// The remote links point at: the current branch's upstream remote when it has one.
    fn permalink_remote(&self) -> Result<String> {
        if let Some(branch) = self.current_branch()? {
            if let Some(remote) = self.config(&format!("branch.{branch}.remote"))? {
                // "." means the branch tracks another local branch, which has no site.
                if remote != "." && self.remotes()?.contains(&remote) {
                    return Ok(remote);
                }
            }
        }
        self.fallback_remote()
    }

    /// The URL `remote` is fetched from, with the user's `url.<base>.insteadOf` rewrites
    /// applied.
    fn remote_url(&self, remote: &str) -> Result<String> {
        let output = self.git().args(["remote", "get-url", "--", remote]).run()?;
        Ok(output.line_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::path_from_bytes;

    const SHA: &str = "8cbc8e3948a4d8437af274ed8924f9a77582e614";

    fn remote(url: &str) -> RemoteUrl {
        parse_remote_url(url).unwrap_or_else(|| panic!("{url} should parse"))
    }

    fn parts(url: &str) -> (String, String, String) {
        let remote = remote(url);
        (remote.scheme, remote.host, remote.path)
    }

    fn expect(scheme: &str, host: &str, path: &str) -> (String, String, String) {
        (scheme.to_string(), host.to_string(), path.to_string())
    }

    fn link(url: &str, path: &str, first: u32, last: u32) -> String {
        build_permalink(&remote(url), SHA, Path::new(path), first, last)
    }

    #[test]
    fn scp_like_urls() {
        let github = expect("https", "github.com", "owner/repo");
        assert_eq!(parts("git@github.com:owner/repo.git"), github);
        assert_eq!(parts("git@github.com:owner/repo"), github);
        assert_eq!(parts("github.com:owner/repo.git"), github);
        assert_eq!(parts("git@github.com:/owner/repo.git/"), github);
        assert_eq!(parts("  git@GitHub.com:owner/repo.git\n"), github);
        assert_eq!(
            parts("org-12345@github.com:Owner/Repo.Name.git"),
            expect("https", "github.com", "Owner/Repo.Name")
        );
        assert_eq!(
            parts("git@gitlab.example.com:group/subgroup/project.git"),
            expect("https", "gitlab.example.com", "group/subgroup/project")
        );
    }

    #[test]
    fn https_urls() {
        let github = expect("https", "github.com", "owner/repo");
        assert_eq!(parts("https://github.com/owner/repo.git"), github);
        assert_eq!(parts("https://github.com/owner/repo"), github);
        assert_eq!(parts("https://github.com/owner/repo/"), github);
        assert_eq!(parts("HTTPS://GitHub.com/owner/repo.git"), github);
        // Credentials never end up in a link.
        assert_eq!(parts("https://user@github.com/owner/repo.git"), github);
        assert_eq!(
            parts("https://user:s3cr3t-t0ken@github.com/owner/repo.git"),
            github
        );
        // The port of an http(s) remote is the web site's port.
        assert_eq!(
            parts("https://git.example.com:8443/team/project.git"),
            expect("https", "git.example.com:8443", "team/project")
        );
        assert_eq!(
            parts("http://gitlab.internal/group/project.git"),
            expect("http", "gitlab.internal", "group/project")
        );
    }

    #[test]
    fn ssh_and_git_urls() {
        let github = expect("https", "github.com", "owner/repo");
        assert_eq!(parts("ssh://git@github.com/owner/repo.git"), github);
        assert_eq!(parts("ssh://github.com/owner/repo"), github);
        assert_eq!(parts("git://github.com/owner/repo.git"), github);
        assert_eq!(parts("git+ssh://git@github.com/owner/repo.git"), github);
        assert_eq!(parts("ssh+git://git@github.com/owner/repo.git"), github);
        // The port is the ssh server's and has no place in a web address.
        assert_eq!(parts("ssh://git@github.com:22/owner/repo.git"), github);
        assert_eq!(
            parts("ssh://git@git.example.com:7999/proj/repo.git"),
            expect("https", "git.example.com", "proj/repo")
        );
    }

    #[test]
    fn urls_that_are_not_a_repository_on_a_host() {
        for url in [
            "",
            "   ",
            "/srv/git/project.git",
            "../relative/project.git",
            "./project",
            "project",
            "file:///srv/git/project.git",
            "C:/Users/someone/project",
            "C:\\Users\\someone\\project",
            "https://github.com",
            "https://github.com/",
            "https:///owner/repo",
            "git@github.com:",
            "git@github.com:.git",
            "ftp://example.com/owner/repo.git",
            "dir/with:colon/repo",
            "git@bad host:owner/repo",
        ] {
            assert_eq!(parse_remote_url(url), None, "{url:?}");
        }
    }

    #[test]
    fn hosts_are_recognised_by_name() {
        let kind = |url: &str| remote(url).host_kind();
        assert_eq!(kind("git@github.com:o/r.git"), HostKind::GitHub);
        assert_eq!(kind("git@github.example.com:o/r.git"), HostKind::GitHub);
        assert_eq!(kind("git@gitlab.com:o/r.git"), HostKind::GitLab);
        assert_eq!(
            kind("https://gitlab.example.com:8443/o/r.git"),
            HostKind::GitLab
        );
        assert_eq!(kind("git@bitbucket.org:o/r.git"), HostKind::Bitbucket);
        assert_eq!(kind("git@codeberg.org:o/r.git"), HostKind::Gitea);
        assert_eq!(kind("https://gitea.example.com/o/r.git"), HostKind::Gitea);
        assert_eq!(kind("https://forgejo.example.com/o/r.git"), HostKind::Gitea);
        // Anything else gets GitHub's format.
        assert_eq!(kind("git@git.example.com:o/r.git"), HostKind::GitHub);
        assert_eq!(
            kind("ssh://git@bitbucket.example.com:7999/o/r.git"),
            HostKind::GitHub
        );
    }

    #[test]
    fn github_links() {
        let url = "git@github.com:owner/repo.git";
        assert_eq!(
            link(url, "src/main.rs", 10, 20),
            format!("https://github.com/owner/repo/blob/{SHA}/src/main.rs#L10-L20")
        );
        assert_eq!(
            link(url, "src/main.rs", 10, 10),
            format!("https://github.com/owner/repo/blob/{SHA}/src/main.rs#L10")
        );
        assert_eq!(
            link("https://github.com/owner/repo", "Cargo.toml", 1, 3),
            format!("https://github.com/owner/repo/blob/{SHA}/Cargo.toml#L1-L3")
        );
    }

    #[test]
    fn gitlab_links() {
        let url = "git@gitlab.com:group/subgroup/project.git";
        assert_eq!(
            link(url, "src/main.rs", 10, 20),
            format!("https://gitlab.com/group/subgroup/project/-/blob/{SHA}/src/main.rs#L10-20")
        );
        assert_eq!(
            link(url, "src/main.rs", 10, 10),
            format!("https://gitlab.com/group/subgroup/project/-/blob/{SHA}/src/main.rs#L10")
        );
        assert_eq!(
            link("http://gitlab.internal:8080/g/p.git", "a.py", 2, 4),
            format!("http://gitlab.internal:8080/g/p/-/blob/{SHA}/a.py#L2-4")
        );
    }

    #[test]
    fn bitbucket_links() {
        let url = "git@bitbucket.org:team/project.git";
        assert_eq!(
            link(url, "src/main.rs", 10, 20),
            format!("https://bitbucket.org/team/project/src/{SHA}/src/main.rs#lines-10:20")
        );
        assert_eq!(
            link(url, "src/main.rs", 10, 10),
            format!("https://bitbucket.org/team/project/src/{SHA}/src/main.rs#lines-10")
        );
    }

    #[test]
    fn gitea_links() {
        assert_eq!(
            link("https://codeberg.org/owner/repo.git", "src/main.rs", 10, 20),
            format!("https://codeberg.org/owner/repo/src/commit/{SHA}/src/main.rs#L10-L20")
        );
        assert_eq!(
            link("git@codeberg.org:owner/repo.git", "src/main.rs", 7, 7),
            format!("https://codeberg.org/owner/repo/src/commit/{SHA}/src/main.rs#L7")
        );
    }

    #[test]
    fn other_hosts_get_github_style_links() {
        assert_eq!(
            link(
                "ssh://git@git.example.com:2222/team/project.git",
                "a/b.c",
                3,
                9
            ),
            format!("https://git.example.com/team/project/blob/{SHA}/a/b.c#L3-L9")
        );
    }

    #[test]
    fn line_numbers_are_ordered_and_start_at_one() {
        let url = "git@github.com:o/r.git";
        let fragment = |first, last| {
            let link = link(url, "f.rs", first, last);
            link.rsplit('#').next().unwrap().to_string()
        };
        assert_eq!(fragment(20, 10), "L10-L20");
        assert_eq!(fragment(0, 0), "L1");
        assert_eq!(fragment(0, 5), "L1-L5");
        assert_eq!(fragment(5, 0), "L1-L5");
        assert_eq!(fragment(1, 1), "L1");
        assert_eq!(fragment(u32::MAX, 1), format!("L1-L{}", u32::MAX));
    }

    #[test]
    fn paths_are_percent_encoded_segment_by_segment() {
        let url = "git@github.com:o/r.git";
        let path_of = |path: &str| {
            let link = link(url, path, 1, 1);
            link.strip_prefix(&format!("https://github.com/o/r/blob/{SHA}/"))
                .and_then(|rest| rest.strip_suffix("#L1"))
                .unwrap()
                .to_string()
        };
        assert_eq!(path_of("src/main.rs"), "src/main.rs");
        assert_eq!(
            path_of("dir with space/a b.txt"),
            "dir%20with%20space/a%20b.txt"
        );
        assert_eq!(
            path_of("app/[id]/(group)/page.tsx"),
            "app/%5Bid%5D/%28group%29/page.tsx"
        );
        assert_eq!(
            path_of("a#b?c%d&e=f+g.txt"),
            "a%23b%3Fc%25d%26e%3Df%2Bg.txt"
        );
        assert_eq!(
            path_of("gr\u{fc}n/\u{65e5}\u{672c}.txt"),
            "gr%C3%BCn/%E6%97%A5%E6%9C%AC.txt"
        );
        assert_eq!(path_of("keep-these_~.chars/x"), "keep-these_~.chars/x");
        assert_eq!(
            path_of("packages/@scope/pkg:1"),
            "packages/%40scope/pkg%3A1"
        );
        assert_eq!(path_of("new\nline\ttab"), "new%0Aline%09tab");

        // Names that are not UTF-8 are encoded byte for byte.
        let raw = path_from_bytes(b"caf\xe9/\xff.txt");
        assert!(build_permalink(&remote(url), SHA, &raw, 1, 1).contains("/caf%E9/%FF.txt#L1"));
    }

    #[test]
    fn markdown_files_ask_for_the_source_view_where_it_matters() {
        assert_eq!(
            link("git@github.com:o/r.git", "docs/README.md", 3, 5),
            format!("https://github.com/o/r/blob/{SHA}/docs/README.md?plain=1#L3-L5")
        );
        assert_eq!(
            link("git@github.com:o/r.git", "NOTES.MARKDOWN", 3, 3),
            format!("https://github.com/o/r/blob/{SHA}/NOTES.MARKDOWN?plain=1#L3")
        );
        assert_eq!(
            link("git@gitlab.com:o/r.git", "README.md", 3, 5),
            format!("https://gitlab.com/o/r/-/blob/{SHA}/README.md?plain=1#L3-5")
        );
        // Bitbucket and Gitea show the source with line numbers anyway.
        assert!(!link("git@bitbucket.org:o/r.git", "README.md", 3, 5).contains('?'));
        assert!(!link("git@codeberg.org:o/r.git", "README.md", 3, 5).contains('?'));
        // Only the extension counts.
        assert!(!link("git@github.com:o/r.git", "md/file.rs", 1, 1).contains('?'));
        assert!(!link("git@github.com:o/r.git", "readme.mdx", 1, 1).contains('?'));
    }

    #[test]
    fn web_url_of_a_remote() {
        assert_eq!(
            remote("git@github.com:owner/repo.git").web_url(),
            "https://github.com/owner/repo"
        );
        assert_eq!(
            remote("http://git.example.com:8080/a/b/c.git").web_url(),
            "http://git.example.com:8080/a/b/c"
        );
    }
    #[test]
    fn mangled_urls_never_panic_and_never_leak_credentials() {
        let samples = [
            "git@github.com:owner/repo.git",
            "https://user:s3cr3t@github.com/owner/repo.git",
            "ssh://git@git.example.com:7999/proj/repo.git",
            "https://[::1]:8443/team/project.git",
            "ssh://git@[2001:db8::1]:22/team/project.git",
        ];
        let mut random = crate::testing::Lcg(6);
        for round in 0..20_000 {
            let mangled = random.mangle(samples[round % samples.len()].as_bytes());
            let url = String::from_utf8_lossy(&mangled);
            let Some(remote) = parse_remote_url(&url) else {
                continue;
            };
            assert!(!remote.host.is_empty() && !remote.path.is_empty());
            assert!(!remote.host.contains('@'), "{url:?} gave {remote:?}");
            let path = path_from_bytes(&random.mangle(b"dir with space/file \xff.md"));
            let link = build_permalink(&remote, SHA, &path, round as u32, 7);
            assert!(link.starts_with(&remote.web_url()));
            assert!(link.contains('#'));
        }
    }
}
