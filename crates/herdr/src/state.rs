//! The file where collapsed splits are remembered.
//!
//! herdr has no notion of a collapsed pane, so collapsing is done by squeezing a split to its
//! minimum ratio and writing down what the ratio was. The user's `herdr-collapse` script
//! keeps that record in `~/.cache/herdr-collapse/state.json`, and their herdr plugin keeps
//! running that script on focus changes, so this module reads and writes the very same file
//! in the very same format: a JSON array of entries, written the way Python's `json.dump`
//! writes it (`", "` and `": "` separators, non-ASCII escaped, no trailing newline), keys in
//! the script's order.
//!
//! Like the script, nothing locks the file: two writers at once means the last one wins.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::client::{Client, HerdrError};
use crate::layout::Node;

/// One remembered split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// The tab the split is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tab_id: Option<String>,
    /// The pane that was squeezed, or the first pane of the squeezed side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pane_id: Option<String>,
    /// Every pane on the squeezed side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) panes: Option<Vec<String>>,
    /// The split's path from the tab's root.
    #[serde(default)]
    pub(crate) path: Vec<bool>,
    /// Which side is squeezed: `true` for the second child.
    #[serde(default)]
    pub(crate) side: bool,
    /// The split's ratio before anything was squeezed.
    pub(crate) ratio: f64,
    /// Whether the entry came from expanding a pane rather than collapsing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) from_expand: Option<bool>,
    /// Anything else a newer writer put in the entry, kept as it is.
    #[serde(flatten)]
    pub(crate) extra: Map<String, Value>,
}

impl Entry {
    /// An entry for the split at `path` of tab `tab_id` whose `side` holds `panes`, the first
    /// of which stands for the squeezed side.
    pub(crate) fn new(
        tab_id: &str,
        panes: &[&str],
        path: &[bool],
        side: bool,
        ratio: f64,
        from_expand: Option<bool>,
    ) -> Entry {
        Entry {
            tab_id: Some(tab_id.to_string()),
            pane_id: panes.first().map(|pane| pane.to_string()),
            panes: Some(panes.iter().map(|pane| pane.to_string()).collect()),
            path: path.to_vec(),
            side,
            ratio,
            from_expand,
            extra: Map::new(),
        }
    }

    /// Whether the entry belongs to tab `tab_id`.
    pub(crate) fn in_tab(&self, tab_id: &str) -> bool {
        self.tab_id.as_deref() == Some(tab_id)
    }

    /// Whether the entry is about the split at `path` of tab `tab_id`.
    pub(crate) fn is_for(&self, tab_id: &str, path: &[bool]) -> bool {
        self.in_tab(tab_id) && self.path == path
    }

    /// Point the entry at a different squeezed side of the same split, keeping the ratio it
    /// remembers.
    pub(crate) fn retarget(&mut self, panes: &[&str], side: bool) {
        self.pane_id = panes.first().map(|pane| pane.to_string());
        self.panes = Some(panes.iter().map(|pane| pane.to_string()).collect());
        self.side = side;
    }

    /// Whether the entry still describes `root`: the split is still there and at least one
    /// of the panes the entry tracked is still on the side it recorded.
    pub(crate) fn is_valid_in(&self, root: &Node) -> bool {
        let Some(split) = root.split_at(&self.path) else {
            return false;
        };
        let side = split.child(self.side);
        let listed = self.panes.iter().flatten().map(String::as_str);
        let named = self.pane_id.as_deref().filter(|pane| !pane.is_empty());
        listed.chain(named).any(|pane| side.contains(pane))
    }
}

/// A collapse state file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StateFile {
    path: PathBuf,
}

impl StateFile {
    /// The state file `client` was given, or [`HerdrError::NoStateFile`].
    pub(crate) fn of(client: &Client) -> Result<StateFile, HerdrError> {
        match client.state_file() {
            Some(path) => Ok(StateFile {
                path: path.to_path_buf(),
            }),
            None => Err(HerdrError::NoStateFile),
        }
    }

    /// Every entry in the file. A missing, unreadable or malformed file is an empty list, and
    /// an entry that is not in the expected shape is left out.
    pub(crate) fn load(&self) -> Vec<Entry> {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return Vec::new();
        };
        let Ok(Value::Array(items)) = serde_json::from_slice::<Value>(&bytes) else {
            return Vec::new();
        };
        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            match serde_json::from_value::<Entry>(item) {
                Ok(entry) => entries.push(entry),
                Err(err) => log::debug!(
                    "dropping a malformed entry of {}: {err}",
                    self.path.display()
                ),
            }
        }
        entries
    }

    /// Replace the file's contents with `entries`, creating its directory if needed. The new
    /// contents are written beside the file and renamed over it, so a reader never sees half
    /// a file.
    pub(crate) fn save(&self, entries: &[Entry]) -> Result<(), HerdrError> {
        let failed =
            |err: io::Error| HerdrError::io(format!("writing {}", self.path.display()), err);
        let bytes = python_json(entries).map_err(failed)?;
        if let Some(dir) = self.path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        let scratch = self.scratch_path();
        let written =
            std::fs::write(&scratch, bytes).and_then(|()| std::fs::rename(&scratch, &self.path));
        if written.is_err() {
            let _ = std::fs::remove_file(&scratch);
        }
        written.map_err(failed)
    }

    /// A name beside the state file that no other writer is using: the script's
    /// `state.json.tmp-<pid>` plus a counter, because this process may have several threads.
    fn scratch_path(&self) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(
            ".tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        self.path.with_file_name(name)
    }

    #[cfg(test)]
    pub(crate) fn at(path: &std::path::Path) -> StateFile {
        StateFile {
            path: path.to_path_buf(),
        }
    }
}

/// `value` as Python's `json.dump(value, file)` would write it.
fn python_json<T: Serialize + ?Sized>(value: &T) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, PythonStyle);
    value.serialize(&mut serializer).map_err(io::Error::from)?;
    Ok(bytes)
}

/// Python's default JSON layout: one line, a space after every `,` and `:`, and everything
/// outside ASCII written as `\uXXXX`.
struct PythonStyle;

impl serde_json::ser::Formatter for PythonStyle {
    fn begin_array_value<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        writer.write_all(b": ")
    }

    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        let mut plain_from = 0;
        for (index, ch) in fragment.char_indices() {
            if ch.is_ascii() {
                continue;
            }
            writer.write_all(&fragment.as_bytes()[plain_from..index])?;
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                write!(writer, "\\u{unit:04x}")?;
            }
            plain_from = index + ch.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[plain_from..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::testing::{down, pane, right};
    use serde_json::json;
    use std::path::Path;

    fn tree(value: Value) -> Node {
        serde_json::from_value(value).unwrap()
    }

    fn text(entries: &[Entry]) -> String {
        String::from_utf8(python_json(entries).unwrap()).unwrap()
    }

    #[test]
    fn writes_exactly_what_the_python_script_writes() {
        // The three shapes the script produces: a collapse, an expand and an auto-expand.
        let entries = vec![
            Entry::new("w1:t1", &["w1:p1"], &[], false, 0.5, Some(false)),
            Entry::new(
                "w1:t1",
                &["w1:p3", "w1:p4"],
                &[true, false],
                true,
                0.3,
                Some(true),
            ),
            Entry::new("w2:t1", &["w2:p2"], &[true], false, 0.5, None),
        ];
        assert_eq!(
            text(&entries),
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "w1:p1", "panes": ["w1:p1"], "path": [], "side": false, "ratio": 0.5, "from_expand": false}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "w1:p3", "panes": ["w1:p3", "w1:p4"], "path": [true, false], "side": true, "ratio": 0.3, "from_expand": true}, "#,
                r#"{"tab_id": "w2:t1", "pane_id": "w2:p2", "panes": ["w2:p2"], "path": [true], "side": false, "ratio": 0.5}]"#
            )
        );
        assert_eq!(text(&[]), "[]");
    }

    #[test]
    fn floats_are_written_like_python_writes_them() {
        for (ratio, expected) in [
            (0.5, "0.5"),
            (0.1, "0.1"),
            (0.9, "0.9"),
            (0.3, "0.3"),
            (1.0 - 0.1, "0.9"),
            (0.12345679, "0.12345679"),
            (0.30000001192092896, "0.30000001192092896"),
            (1.0, "1.0"),
            (0.7000000000000001, "0.7000000000000001"),
        ] {
            let written = text(&[Entry::new("t", &["p"], &[], false, ratio, None)]);
            assert!(
                written.contains(&format!("\"ratio\": {expected}}}")),
                "{ratio}: {written}"
            );
        }
    }

    #[test]
    fn strings_are_escaped_like_python_escapes_them() {
        let entry = Entry::new(
            "w1:t1",
            &["caf\u{e9} \u{1F600} \"q\" \\ \n\t\u{1}"],
            &[],
            false,
            0.5,
            None,
        );
        let written = text(&[entry]);
        // `%` stands for a backslash so that the escapes below are not escapes of this file.
        let expected =
            r#""pane_id": "caf%u00e9 %ud83d%ude00 %"q%" %% %n%t%u0001""#.replace('%', "\\");
        assert!(written.contains(&expected), "{written}");
        assert!(written.is_ascii());
    }

    #[test]
    fn a_file_written_by_the_script_round_trips_unchanged() {
        let from_python = concat!(
            r#"[{"tab_id": "w1:t1", "pane_id": "w1:p1", "panes": ["w1:p1"], "path": [false], "side": false, "ratio": 0.5, "from_expand": false}, "#,
            r#"{"tab_id": "w3:t2", "pane_id": "w3:pA", "panes": ["w3:pA", "w3:pB"], "path": [], "side": true, "ratio": 0.42, "from_expand": true}, "#,
            r#"{"tab_id": "w4:t1", "pane_id": "w4:p2", "panes": ["w4:p2"], "path": [true, true], "side": true, "ratio": 0.5}]"#
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, from_python).unwrap();
        let file = StateFile::at(&path);
        let entries = file.load();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[1].panes.as_deref(),
            Some(&["w3:pA".to_string(), "w3:pB".to_string()][..])
        );
        assert_eq!(entries[2].from_expand, None);
        file.save(&entries).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), from_python);
    }

    #[test]
    fn unknown_keys_survive_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"[{"tab_id": "t", "path": [], "side": true, "ratio": 0.5, "note": {"k": [1, 2]}}]"#,
        )
        .unwrap();
        let file = StateFile::at(&path);
        let entries = file.load();
        assert_eq!(entries[0].extra.get("note"), Some(&json!({"k": [1, 2]})));
        assert_eq!(entries[0].pane_id, None);
        file.save(&entries).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"[{"tab_id": "t", "path": [], "side": true, "ratio": 0.5, "note": {"k": [1, 2]}}]"#
        );
    }

    #[test]
    fn a_missing_or_broken_file_is_an_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        let file = StateFile::at(&dir.path().join("nope").join("state.json"));
        assert_eq!(file.load(), vec![]);
        for contents in ["", "not json", "{\"tab_id\": \"t\"}", "42", "null", "[1, 2"] {
            let path = dir.path().join("state.json");
            std::fs::write(&path, contents).unwrap();
            assert_eq!(StateFile::at(&path).load(), vec![], "{contents:?}");
        }
    }

    #[test]
    fn entries_in_the_wrong_shape_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"[1, "x", null, {"tab_id": "t"}, {"tab_id": "t", "ratio": "half"}, {"tab_id": "t", "ratio": 0.5, "path": "root"},
               {"tab_id": "keep", "ratio": 1}, {"ratio": 0.25, "side": true, "path": [true]}]"#,
        )
        .unwrap();
        let entries = StateFile::at(&path).load();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].tab_id.as_deref(), Some("keep"));
        assert_eq!(entries[0].ratio, 1.0);
        assert_eq!(entries[0].path, Vec::<bool>::new());
        assert!(!entries[0].side);
        assert_eq!(entries[1].tab_id, None);
        assert!(!entries[1].in_tab("keep"));
    }

    #[test]
    fn save_creates_the_directory_and_leaves_no_scratch_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("cache")
            .join("herdr-collapse")
            .join("state.json");
        let file = StateFile::at(&path);
        file.save(&[Entry::new("t", &["p"], &[], false, 0.5, None)])
            .unwrap();
        file.save(&[]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["state.json"]);
    }

    #[test]
    fn save_reports_where_it_failed() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "a file where a directory should be").unwrap();
        let err = StateFile::at(&blocker.join("state.json"))
            .save(&[])
            .unwrap_err();
        assert!(
            matches!(&err, HerdrError::Io { context, .. } if context.contains("state.json")),
            "{err:?}"
        );
    }

    #[test]
    fn concurrent_saves_do_not_trip_over_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::thread::scope(|scope| {
            for worker in 0..8 {
                let path = path.clone();
                scope.spawn(move || {
                    for round in 0..25 {
                        let entry = Entry::new(
                            &format!("w{worker}:t{round}"),
                            &["p"],
                            &[],
                            false,
                            0.5,
                            None,
                        );
                        StateFile::at(&path).save(&[entry]).unwrap();
                    }
                });
            }
        });
        assert_eq!(StateFile::at(&path).load().len(), 1);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn the_state_file_comes_from_the_client() {
        let client = Client::new("/nowhere/herdr.sock");
        assert!(matches!(
            StateFile::of(&client).unwrap_err(),
            HerdrError::NoStateFile
        ));
        let client = client.with_state_file("/tmp/somewhere/state.json");
        assert_eq!(
            StateFile::of(&client).unwrap(),
            StateFile::at(Path::new("/tmp/somewhere/state.json"))
        );
    }

    #[test]
    fn an_entry_is_valid_while_a_tracked_pane_stays_on_its_side() {
        // (a | (b / c))
        let root = tree(right(0.9, pane("a"), down(0.5, pane("b"), pane("c"))));
        let entry = |panes: &[&str], path: &[bool], side: bool| {
            Entry::new("t", panes, path, side, 0.5, None)
        };
        assert!(entry(&["b", "c"], &[], true).is_valid_in(&root));
        assert!(entry(&["gone", "c"], &[], true).is_valid_in(&root));
        assert!(entry(&["a"], &[], false).is_valid_in(&root));
        assert!(entry(&["c"], &[true], true).is_valid_in(&root));
        // The pane moved to the other side, or left the tab.
        assert!(!entry(&["a"], &[], true).is_valid_in(&root));
        assert!(!entry(&["gone"], &[], false).is_valid_in(&root));
        // The path no longer ends at a split.
        assert!(!entry(&["a"], &[false], false).is_valid_in(&root));
        assert!(!entry(&["c"], &[true, true, false], true).is_valid_in(&root));
        assert!(!entry(&["a"], &[], false).is_valid_in(&tree(pane("a"))));
    }

    #[test]
    fn validity_uses_the_named_pane_when_no_list_was_stored() {
        let root = tree(right(0.5, pane("a"), pane("b")));
        let mut entry = Entry::new("t", &["a"], &[], false, 0.5, None);
        entry.panes = None;
        assert!(entry.is_valid_in(&root));
        entry.pane_id = Some(String::new());
        assert!(!entry.is_valid_in(&root));
        entry.pane_id = None;
        assert!(!entry.is_valid_in(&root));
    }

    #[test]
    fn retarget_keeps_the_remembered_ratio() {
        let mut entry = Entry::new("t", &["a"], &[true], false, 0.3, Some(false));
        entry.retarget(&["b", "c"], true);
        assert_eq!(entry.pane_id.as_deref(), Some("b"));
        assert_eq!(entry.panes, Some(vec!["b".to_string(), "c".to_string()]));
        assert!(entry.side);
        assert_eq!(entry.ratio, 0.3);
        assert_eq!(entry.from_expand, Some(false));
        assert!(entry.is_for("t", &[true]));
        assert!(!entry.is_for("t", &[]));
        assert!(!entry.is_for("other", &[true]));
    }
}
