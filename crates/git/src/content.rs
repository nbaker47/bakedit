//! File contents as git has them: at `HEAD` and in the index.

use std::ffi::OsString;
use std::path::Path;

use crate::error::Result;
use crate::paths::{literal_pathspec, path_bytes};
use crate::repo::Repo;

/// The file mode of a submodule entry, which has an object name but no content here.
const SUBMODULE_MODE: &[u8] = b"160000";

impl Repo {
    /// The content of `path` in the current commit, as the bytes a checkout would write
    /// to the work tree (line ending conversion and other configured filters applied),
    /// so it can be compared with the file on disk. `None` when the commit has no such
    /// file: it is new, the path is a directory or a submodule, or there is no commit
    /// yet. For a symbolic link the content is the path it points to.
    pub fn head_bytes(&self, path: impl AsRef<Path>) -> Result<Option<Vec<u8>>> {
        let relative = self.relative_path(path)?;
        match self.head_blob(&relative)? {
            Some(object) => self.blob_content(&object, &relative).map(Some),
            None => Ok(None),
        }
    }

    /// The staged content of `path`, as the bytes a checkout would write to the work
    /// tree. `None` when the index has no such file: it is untracked, deleted from the
    /// index, a directory or a submodule, or it is conflicted and so has no single staged
    /// version.
    pub fn index_bytes(&self, path: impl AsRef<Path>) -> Result<Option<Vec<u8>>> {
        let relative = self.relative_path(path)?;
        match self.index_blob(&relative)? {
            Some(object) => self.blob_content(&object, &relative).map(Some),
            None => Ok(None),
        }
    }

    /// [`Repo::head_bytes`] as text, for the editor gutter. Bytes that are not valid
    /// UTF-8 are replaced by U+FFFD; use the bytes when the file may be in another
    /// encoding.
    pub fn head_text(&self, path: impl AsRef<Path>) -> Result<Option<String>> {
        Ok(self.head_bytes(path)?.map(lossy_text))
    }

    /// [`Repo::index_bytes`] as text, for the editor gutter. Bytes that are not valid
    /// UTF-8 are replaced by U+FFFD.
    pub fn index_text(&self, path: impl AsRef<Path>) -> Result<Option<String>> {
        Ok(self.index_bytes(path)?.map(lossy_text))
    }

    /// The object that holds the file `relative` in the current commit, if it has one.
    fn head_blob(&self, relative: &Path) -> Result<Option<String>> {
        if relative.as_os_str().is_empty() {
            return Ok(None);
        }
        let listing = self
            .git()
            .args(["ls-tree", "-z", "HEAD", "--"])
            .arg(literal_pathspec(relative))
            .run();
        let output = match listing {
            Ok(output) => output,
            // Without a commit there is no tree to look in.
            Err(error) => {
                return match self.head_sha()? {
                    None => Ok(None),
                    Some(_) => Err(error),
                }
            }
        };
        // <mode> SP <type> SP <object> TAB <path>
        let object = records_for(&output.stdout, relative).find_map(|fields| match fields[..] {
            [_, b"blob", object] => Some(String::from_utf8_lossy(object).into_owned()),
            _ => None,
        });
        Ok(object)
    }

    /// The object that holds the staged version of the file `relative`, if there is
    /// exactly one.
    fn index_blob(&self, relative: &Path) -> Result<Option<String>> {
        if relative.as_os_str().is_empty() {
            return Ok(None);
        }
        let output = self
            .git()
            .args(["ls-files", "-z", "--stage", "--"])
            .arg(literal_pathspec(relative))
            .run()?;
        // <mode> SP <object> SP <stage> TAB <path>; a conflicted file has stages 1 to 3
        // and no stage 0.
        let object = records_for(&output.stdout, relative).find_map(|fields| match fields[..] {
            [mode, object, b"0"] if mode != SUBMODULE_MODE => {
                Some(String::from_utf8_lossy(object).into_owned())
            }
            _ => None,
        });
        Ok(object)
    }

    /// The content of the blob `object`, filtered the way git would check out `relative`.
    fn blob_content(&self, object: &str, relative: &Path) -> Result<Vec<u8>> {
        let mut path_option = OsString::from("--path=");
        path_option.push(relative);
        let output = self
            .git()
            .args(["cat-file", "--filters"])
            .arg(path_option)
            .arg(object)
            .run()?;
        Ok(output.stdout)
    }
}

/// The records of a NUL-separated `ls-tree` or `ls-files --stage` listing that are about
/// exactly `path` (a directory lists the files below it, which are not it), each as the
/// space-separated fields before the tab.
fn records_for<'a>(listing: &'a [u8], path: &'a Path) -> impl Iterator<Item = Vec<&'a [u8]>> {
    listing.split(|&byte| byte == 0).filter_map(move |record| {
        let tab = record.iter().position(|&byte| byte == b'\t')?;
        (&record[tab + 1..] == path_bytes(path))
            .then(|| record[..tab].split(|&byte| byte == b' ').collect())
    })
}

fn lossy_text(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJECT: &str = "78981922613b2afb6025042ff6bd878ac1994e85";

    fn fields_of(listing: &[u8], path: &str) -> Vec<Vec<String>> {
        records_for(listing, Path::new(path))
            .map(|fields| {
                fields
                    .iter()
                    .map(|field| String::from_utf8_lossy(field).into_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn text_is_taken_as_is_when_it_is_utf8() {
        assert_eq!(
            lossy_text("gr\u{fc}n\r\n".as_bytes().to_vec()),
            "gr\u{fc}n\r\n"
        );
        assert_eq!(lossy_text(Vec::new()), "");
    }

    #[test]
    fn other_encodings_are_replaced_not_dropped() {
        assert_eq!(lossy_text(b"caf\xe9\n".to_vec()), "caf\u{fffd}\n");
    }

    #[test]
    fn listing_records_are_matched_by_their_exact_path() {
        let listing = format!(
            "100644 blob {OBJECT}\tsrc/lib.rs\0100644 blob {OBJECT}\tsrc/lib.rs.bak\0\
             040000 tree {OBJECT}\tsrc\0"
        );
        assert_eq!(
            fields_of(listing.as_bytes(), "src/lib.rs"),
            [["100644", "blob", OBJECT]]
        );
        assert_eq!(
            fields_of(listing.as_bytes(), "src"),
            [["040000", "tree", OBJECT]]
        );
        assert!(fields_of(listing.as_bytes(), "src/lib").is_empty());
        assert!(fields_of(b"", "src/lib.rs").is_empty());
        assert!(fields_of(b"no tab here\0", "here").is_empty());
    }

    #[test]
    fn paths_with_tabs_and_line_feeds_are_matched_whole() {
        let listing = format!("100644 {OBJECT} 0\tta\tb\nc.txt\0");
        assert_eq!(
            fields_of(listing.as_bytes(), "ta\tb\nc.txt"),
            [["100644", OBJECT, "0"]]
        );
        assert!(fields_of(listing.as_bytes(), "b\nc.txt").is_empty());
    }
}
