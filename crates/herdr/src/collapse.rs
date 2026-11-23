//! Collapsing and expanding panes: a port of the `herdr-collapse` script.
//!
//! herdr cannot hide a pane, so a pane is "collapsed" by squeezing the split it sits in down
//! to herdr's minimum ratio, and "expanded" by squeezing everything around it instead. What
//! the ratios were before is written to the collapse state file (see [`crate::Client`]), in
//! the script's own format, so a pane collapsed by one can be restored by the other.

use crate::client::{Client, HerdrError};
use crate::layout::{self, Node, Split, TabLayout};
use crate::state::{Entry, StateFile};

/// The smallest share herdr gives one side of a split: it clamps ratios to 0.1..=0.9.
const MIN_RATIO: f64 = 0.1;

/// A side holding at most this share of its split counts as collapsed.
const SLIVER_THRESHOLD: f64 = MIN_RATIO + 0.02;

/// Two ratios closer than this are the same ratio.
const SAME_RATIO: f64 = 0.02;

/// What [`collapse`] should do with the focused pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CollapseMode {
    /// Collapse the pane to a sliver, or, when its tab already has collapsed or expanded
    /// splits, put all of them back.
    Toggle,
    /// Grow the pane to 90% of every split above it, or put everything back when it already
    /// is.
    Expand,
    /// Run after focus lands on a pane: if the pane is inside a collapsed split, flip the
    /// split so the pane is the large side.
    Auto,
}

/// What [`collapse`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollapseOutcome {
    /// The pane was squeezed to a sliver and focus moved to its neighbour.
    Collapsed {
        /// The pane that was collapsed.
        pane_id: String,
        /// The pane that has the focus now.
        focused_pane_id: String,
    },
    /// Splits that had been squeezed are back at the ratios they had before.
    Restored {
        /// How many splits were put back.
        splits: usize,
        /// The pane that has the focus now: the pane that had been collapsed when a single
        /// collapse was undone, otherwise the pane that was focused already.
        focused_pane_id: String,
    },
    /// The pane now takes 90% of every split above it.
    Expanded {
        /// The pane that was expanded.
        pane_id: String,
        /// How many splits are above it.
        splits: usize,
    },
    /// The pane was expanded already and nothing was on record for its tab, so the splits
    /// above it were set to half and half.
    Evened {
        /// The pane that had been expanded.
        pane_id: String,
        /// How many splits were evened out.
        splits: usize,
    },
    /// The pane sat in collapsed splits, which were flipped so it is the large side.
    AutoExpanded {
        /// The pane that was given the room.
        pane_id: String,
        /// How many splits were flipped.
        splits: usize,
    },
    /// The tab has a single pane: there is nothing to collapse or expand.
    SinglePane,
    /// The pane is a sliver already and nothing is on record to restore.
    AlreadyMinimum,
    /// There was nothing to do: no pane is focused, or, in [`CollapseMode::Auto`], the pane
    /// is not in a collapsed split or is not the focused pane of its tab.
    Unchanged,
}

impl CollapseOutcome {
    /// Whether anything in herdr was changed.
    pub fn changed(&self) -> bool {
        !matches!(
            self,
            CollapseOutcome::SinglePane
                | CollapseOutcome::AlreadyMinimum
                | CollapseOutcome::Unchanged
        )
    }
}

/// Collapse, expand or auto-expand the focused pane of the tab on screen.
///
/// [`CollapseMode::Toggle`] and [`CollapseMode::Expand`] need the client's collapse state
/// file and fail with [`HerdrError::NoStateFile`] without one; [`CollapseMode::Auto`] needs
/// it only when it has a split to flip. An empty session is [`CollapseOutcome::Unchanged`].
/// When a herdr call fails part-way the error is returned; splits already changed stay
/// changed and, for `Expand` and `Auto`, are on record so the next toggle puts them back.
pub fn collapse(client: &Client, mode: CollapseMode) -> Result<CollapseOutcome, HerdrError> {
    run(client, mode, None)
}

/// [`collapse`] for the tab holding `pane_id`, treating that pane as the focused one.
///
/// This is what the script does when `HERDR_COLLAPSE_PANE` is set, which is how the user's
/// auto-expand plugin calls it with the pane a focus event named. In [`CollapseMode::Auto`]
/// nothing happens unless `pane_id` really is the focused pane of its tab, so a stale event
/// cannot rearrange a tab the user has moved on from. An unknown pane is an error.
pub fn collapse_pane(
    client: &Client,
    mode: CollapseMode,
    pane_id: &str,
) -> Result<CollapseOutcome, HerdrError> {
    run(client, mode, Some(pane_id))
}

fn run(
    client: &Client,
    mode: CollapseMode,
    target: Option<&str>,
) -> Result<CollapseOutcome, HerdrError> {
    let layout = match layout::export(client, target) {
        Ok(layout) => layout,
        Err(err) if target.is_none() && layout::nothing_focused(&err) => {
            return Ok(CollapseOutcome::Unchanged)
        }
        Err(err) => return Err(err),
    };
    if mode == CollapseMode::Auto {
        if target.is_some_and(|pane| pane != layout.focused_pane_id) {
            return Ok(CollapseOutcome::Unchanged);
        }
        return auto_expand(client, &layout);
    }

    let focused = target.unwrap_or(&layout.focused_pane_id);
    let store = StateFile::of(client)?;
    let state = store.load();
    let Some(path) = layout
        .root
        .find_path(focused)
        .filter(|path| !path.is_empty())
    else {
        // Whatever was on record for this tab described splits that are gone.
        if state.iter().any(|entry| entry.in_tab(&layout.tab_id)) {
            store.save(&without_tab(state, &layout.tab_id))?;
        }
        return Ok(CollapseOutcome::SinglePane);
    };
    let job = Job {
        client,
        store: &store,
        layout: &layout,
        focused,
    };
    match mode {
        CollapseMode::Expand => job.expand(&path, state),
        _ => job.toggle(&path, state),
    }
}

/// One collapse or expand of pane `focused` in the tab `layout` describes.
struct Job<'a> {
    client: &'a Client,
    store: &'a StateFile,
    layout: &'a TabLayout,
    focused: &'a str,
}

impl Job<'_> {
    fn tab(&self) -> &str {
        &self.layout.tab_id
    }

    fn set_ratio(&self, path: &[bool], ratio: f64) -> Result<(), HerdrError> {
        layout::set_split_ratio(self.client, path, ratio, self.focused)
    }

    /// Restore whatever is on record for the tab; failing that, squeeze the focused pane and
    /// hand the focus to its neighbour.
    fn toggle(&self, path: &[bool], state: Vec<Entry>) -> Result<CollapseOutcome, HerdrError> {
        if let Some(outcome) = self.restore(&state, false)? {
            return Ok(outcome);
        }
        let Some((&side, parent_path)) = path.split_last() else {
            return Ok(CollapseOutcome::SinglePane);
        };
        let Some(parent) = self.layout.root.split_at(parent_path) else {
            return Ok(CollapseOutcome::SinglePane);
        };
        let squeezed = if side { 1.0 - MIN_RATIO } else { MIN_RATIO };
        if (parent.ratio - squeezed).abs() < SAME_RATIO {
            return Ok(CollapseOutcome::AlreadyMinimum);
        }
        self.set_ratio(parent_path, squeezed)?;

        let mut state = without_tab(state, self.tab());
        let own_side = parent.child(side).leaves();
        state.push(Entry::new(
            self.tab(),
            &own_side,
            parent_path,
            side,
            parent.ratio,
            Some(false),
        ));
        self.store.save(&state)?;

        let neighbour = parent.child(!side).first_leaf();
        layout::focus_pane(self.client, neighbour)?;
        Ok(CollapseOutcome::Collapsed {
            pane_id: self.focused.to_string(),
            focused_pane_id: neighbour.to_string(),
        })
    }

    /// Give the focused pane 90% of every split above it, or undo that when it has it.
    fn expand(&self, path: &[bool], state: Vec<Entry>) -> Result<CollapseOutcome, HerdrError> {
        let ancestors = ancestors(&self.layout.root, path);
        let expanded_already = !ancestors.is_empty()
            && ancestors.iter().all(|ancestor| {
                (ancestor.split.ratio - ancestor.expanded_ratio()).abs() < SAME_RATIO
            });
        if expanded_already {
            if let Some(outcome) = self.restore(&state, true)? {
                return Ok(outcome);
            }
            for ancestor in &ancestors {
                self.set_ratio(ancestor.path, 0.5)?;
            }
            return Ok(CollapseOutcome::Evened {
                pane_id: self.focused.to_string(),
                splits: ancestors.len(),
            });
        }

        // Entries that still describe the tab keep the ratios from before anything was
        // squeezed; stale ones are dropped.
        let (mine, mut state): (Vec<Entry>, Vec<Entry>) = state
            .into_iter()
            .partition(|entry| entry.in_tab(self.tab()));
        state.extend(
            mine.into_iter()
                .filter(|entry| entry.is_valid_in(&self.layout.root)),
        );

        let mut failure = None;
        for ancestor in &ancestors {
            let squeezed_side = ancestor.split.child(!ancestor.side).leaves();
            match state
                .iter_mut()
                .rev()
                .find(|entry| entry.is_for(self.tab(), ancestor.path))
            {
                Some(entry) => {
                    entry.retarget(&squeezed_side, !ancestor.side);
                    entry.from_expand = Some(true);
                }
                None => state.push(Entry::new(
                    self.tab(),
                    &squeezed_side,
                    ancestor.path,
                    !ancestor.side,
                    ancestor.split.ratio,
                    Some(true),
                )),
            }
            if (ancestor.split.ratio - ancestor.expanded_ratio()).abs() >= SAME_RATIO {
                if let Err(err) = self.set_ratio(ancestor.path, ancestor.expanded_ratio()) {
                    failure = Some(err);
                    break;
                }
            }
        }
        let saved = self.store.save(&state);
        if let Some(err) = failure {
            return Err(err);
        }
        saved?;
        Ok(CollapseOutcome::Expanded {
            pane_id: self.focused.to_string(),
            splits: ancestors.len(),
        })
    }

    /// Put every split on record for this tab back to its remembered ratio and forget the
    /// tab's entries. `None` when nothing on record still describes the tab.
    fn restore(
        &self,
        state: &[Entry],
        after_expand: bool,
    ) -> Result<Option<CollapseOutcome>, HerdrError> {
        let valid: Vec<&Entry> = state
            .iter()
            .filter(|entry| entry.in_tab(self.tab()) && entry.is_valid_in(&self.layout.root))
            .collect();
        let mut restored: Vec<&[bool]> = Vec::new();
        for entry in &valid {
            if restored.contains(&entry.path.as_slice()) {
                continue;
            }
            restored.push(&entry.path);
            self.set_ratio(&entry.path, entry.ratio)?;
        }
        let others: Vec<Entry> = state
            .iter()
            .filter(|entry| !entry.in_tab(self.tab()))
            .cloned()
            .collect();
        self.store.save(&others)?;

        let Some(last) = valid.last() else {
            return Ok(None);
        };
        // Undoing one plain collapse hands the focus back to the pane that was collapsed,
        // as long as it is still there.
        let collapsed = last
            .pane_id
            .as_deref()
            .filter(|pane| self.layout.root.contains(pane));
        let focus = match collapsed {
            Some(pane) if !after_expand && valid.len() == 1 && last.from_expand != Some(true) => {
                pane
            }
            _ => self.focused,
        };
        layout::focus_pane(self.client, focus)?;
        Ok(Some(CollapseOutcome::Restored {
            splits: restored.len(),
            focused_pane_id: focus.to_string(),
        }))
    }
}

/// Flip every collapsed split above the focused pane so that the pane is its large side.
fn auto_expand(client: &Client, layout: &TabLayout) -> Result<CollapseOutcome, HerdrError> {
    let focused = layout.focused_pane_id.as_str();
    let Some(path) = layout.root.find_path(focused) else {
        return Ok(CollapseOutcome::Unchanged);
    };
    let squeezed: Vec<Ancestor> = ancestors(&layout.root, &path)
        .into_iter()
        .filter(|ancestor| ancestor.share() <= SLIVER_THRESHOLD)
        .collect();
    if squeezed.is_empty() {
        return Ok(CollapseOutcome::Unchanged);
    }

    let store = StateFile::of(client)?;
    let mut state = store.load();
    let mut flipped = 0;
    let mut failure = None;
    for ancestor in &squeezed {
        if let Err(err) =
            layout::set_split_ratio(client, ancestor.path, ancestor.expanded_ratio(), focused)
        {
            failure = Some(err);
            break;
        }
        // Whichever side was large is the squeezed one now. An existing entry keeps the
        // ratio from before the collapse; a split squeezed by other means goes back to half.
        let squeezed_side = ancestor.split.child(!ancestor.side).leaves();
        match state
            .iter_mut()
            .rev()
            .find(|entry| entry.is_for(&layout.tab_id, ancestor.path))
        {
            Some(entry) => entry.retarget(&squeezed_side, !ancestor.side),
            None => state.push(Entry::new(
                &layout.tab_id,
                &squeezed_side,
                ancestor.path,
                !ancestor.side,
                0.5,
                None,
            )),
        }
        flipped += 1;
    }
    let saved = if flipped > 0 {
        store.save(&state)
    } else {
        Ok(())
    };
    if let Some(err) = failure {
        return Err(err);
    }
    saved?;
    Ok(CollapseOutcome::AutoExpanded {
        pane_id: focused.to_string(),
        splits: flipped,
    })
}

/// A split above a pane, and which of its sides the pane is on.
struct Ancestor<'a> {
    /// The split's path from the root.
    path: &'a [bool],
    /// The side holding the pane: `true` for the second child.
    side: bool,
    split: Split<'a>,
}

impl Ancestor<'_> {
    /// The share of the split the pane's side has.
    fn share(&self) -> f64 {
        if self.side {
            1.0 - self.split.ratio
        } else {
            self.split.ratio
        }
    }

    /// The ratio that gives the pane's side all the room herdr allows.
    fn expanded_ratio(&self) -> f64 {
        if self.side {
            MIN_RATIO
        } else {
            1.0 - MIN_RATIO
        }
    }
}

/// The splits on the way from `root` down to the pane at `path`, outermost first.
fn ancestors<'a>(root: &'a Node, path: &'a [bool]) -> Vec<Ancestor<'a>> {
    (0..path.len())
        .filter_map(|depth| {
            let split = root.split_at(&path[..depth])?;
            Some(Ancestor {
                path: &path[..depth],
                side: path[depth],
                split,
            })
        })
        .collect()
}

fn without_tab(state: Vec<Entry>, tab_id: &str) -> Vec<Entry> {
    state
        .into_iter()
        .filter(|entry| !entry.in_tab(tab_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::testing::{down, export_result, pane, right};
    use crate::mock::{MockServer, Request};
    use serde_json::{json, Value};

    const OK: &str = "layout.set_split_ratio";

    fn ok() -> Value {
        json!({"type": "ok"})
    }

    /// A server whose next reply is the layout of tab `w1:t1`.
    fn server_with(focused: &str, root: Value) -> MockServer {
        let server = MockServer::start();
        server.ok("layout.export", export_result(focused, false, root));
        server
    }

    fn write_state(server: &MockServer, json: &str) {
        let path = server.state_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, json).unwrap();
    }

    fn read_state(server: &MockServer) -> String {
        std::fs::read_to_string(server.state_file()).unwrap()
    }

    fn calls(server: &MockServer) -> Vec<(String, Value)> {
        server
            .requests()
            .into_iter()
            .skip(1)
            .map(|Request { method, params, .. }| (method, params))
            .collect()
    }

    fn ratio_call(path: Value, ratio: f64, pane: &str) -> (String, Value) {
        (
            OK.to_string(),
            json!({"path": path, "ratio": ratio, "pane_id": pane}),
        )
    }

    fn focus_call(pane: &str) -> (String, Value) {
        ("pane.focus".to_string(), json!({"pane_id": pane}))
    }

    // ---- toggle ----------------------------------------------------------------------

    #[test]
    fn toggle_collapses_the_first_pane_and_focuses_its_neighbour() {
        let server = server_with("a", right(0.5, pane("a"), pane("b")));
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Collapsed {
                pane_id: "a".into(),
                focused_pane_id: "b".into()
            }
        );
        assert!(outcome.changed());
        assert_eq!(
            calls(&server),
            vec![ratio_call(json!([]), 0.1, "a"), focus_call("b")]
        );
        assert_eq!(
            read_state(&server),
            r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.5, "from_expand": false}]"#
        );
        server.assert_finished();
    }

    #[test]
    fn toggle_collapses_a_second_pane_towards_the_far_edge() {
        // (a | (b / c)) with c focused: only the inner split is squeezed, and the neighbour
        // that gets the focus is the first pane of the other side.
        let server = server_with("c", right(0.5, pane("a"), down(0.3, pane("b"), pane("c"))));
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Collapsed {
                pane_id: "c".into(),
                focused_pane_id: "b".into()
            }
        );
        assert_eq!(
            calls(&server),
            vec![ratio_call(json!([true]), 0.9, "c"), focus_call("b")]
        );
        assert_eq!(
            read_state(&server),
            r#"[{"tab_id": "w1:t1", "pane_id": "c", "panes": ["c"], "path": [true], "side": true, "ratio": 0.3, "from_expand": false}]"#
        );
    }

    #[test]
    fn toggle_focuses_the_first_pane_of_a_neighbouring_subtree() {
        // (a | (b / c)) with a focused: the neighbour is a subtree, its first pane gets focus.
        let server = server_with("a", right(0.5, pane("a"), down(0.5, pane("b"), pane("c"))));
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Collapsed {
                pane_id: "a".into(),
                focused_pane_id: "b".into()
            }
        );
    }

    #[test]
    fn toggle_in_a_single_pane_tab_does_nothing() {
        let server = server_with("a", pane("a"));
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert_eq!(outcome, CollapseOutcome::SinglePane);
        assert!(!outcome.changed());
        assert!(calls(&server).is_empty());
        assert!(
            !server.state_file().exists(),
            "nothing was on record, so nothing is written"
        );
    }

    #[test]
    fn a_single_pane_tab_forgets_what_was_on_record_for_it() {
        let server = server_with("a", pane("a"));
        write_state(
            &server,
            r#"[{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.5, "from_expand": false}, {"tab_id": "w9:t9", "pane_id": "z", "panes": ["z"], "path": [], "side": false, "ratio": 0.4}]"#,
        );
        assert_eq!(
            collapse(&server.client(), CollapseMode::Expand).unwrap(),
            CollapseOutcome::SinglePane
        );
        assert_eq!(
            read_state(&server),
            r#"[{"tab_id": "w9:t9", "pane_id": "z", "panes": ["z"], "path": [], "side": false, "ratio": 0.4}]"#
        );
    }

    #[test]
    fn toggle_on_a_pane_that_is_already_a_sliver_does_nothing() {
        for (ratio, focused) in [(0.1, "a"), (0.11, "a"), (0.9, "b"), (0.885, "b")] {
            let server = server_with(focused, right(ratio, pane("a"), pane("b")));
            let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
            assert_eq!(
                outcome,
                CollapseOutcome::AlreadyMinimum,
                "{ratio} {focused}"
            );
            assert!(calls(&server).is_empty());
            // The restore attempt rewrote the file without this tab's entries: an empty list.
            assert_eq!(read_state(&server), "[]");
        }
    }

    #[test]
    fn toggle_just_outside_the_sliver_band_still_collapses() {
        let server = server_with("a", right(0.13, pane("a"), pane("b")));
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert!(matches!(outcome, CollapseOutcome::Collapsed { .. }));
    }

    #[test]
    fn toggle_restores_a_single_collapse_and_refocuses_the_collapsed_pane() {
        // a was collapsed (ratio 0.1), focus is on b.
        let server = server_with("b", right(0.1, pane("a"), pane("b")));
        write_state(
            &server,
            r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.42, "from_expand": false}]"#,
        );
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Restored {
                splits: 1,
                focused_pane_id: "a".into()
            }
        );
        assert_eq!(
            calls(&server),
            vec![ratio_call(json!([]), 0.42, "b"), focus_call("a")]
        );
        assert_eq!(read_state(&server), "[]");
        server.assert_finished();
    }

    #[test]
    fn toggle_restores_every_split_and_keeps_focus_when_several_are_on_record() {
        let server = server_with("c", right(0.1, pane("a"), down(0.1, pane("b"), pane("c"))));
        write_state(
            &server,
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.5, "from_expand": true}, "#,
                r#"{"tab_id": "w2:t1", "pane_id": "q", "panes": ["q"], "path": [], "side": false, "ratio": 0.7, "from_expand": false}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [true], "side": false, "ratio": 0.3, "from_expand": true}]"#
            ),
        );
        server.ok(OK, ok()).ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Restored {
                splits: 2,
                focused_pane_id: "c".into()
            }
        );
        assert_eq!(
            calls(&server),
            vec![
                ratio_call(json!([]), 0.5, "c"),
                ratio_call(json!([true]), 0.3, "c"),
                focus_call("c")
            ]
        );
        // Only the other tab's entry is left.
        assert_eq!(
            read_state(&server),
            r#"[{"tab_id": "w2:t1", "pane_id": "q", "panes": ["q"], "path": [], "side": false, "ratio": 0.7, "from_expand": false}]"#
        );
    }

    #[test]
    fn toggle_after_an_expand_keeps_the_focus_where_it_is() {
        // One entry, but it came from an expand: the focused pane stays focused.
        let server = server_with("a", right(0.9, pane("a"), pane("b")));
        write_state(
            &server,
            r#"[{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.5, "from_expand": true}]"#,
        );
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Restored {
                splits: 1,
                focused_pane_id: "a".into()
            }
        );
        assert_eq!(
            calls(&server),
            vec![ratio_call(json!([]), 0.5, "a"), focus_call("a")]
        );
    }

    #[test]
    fn toggle_after_an_auto_expand_refocuses_the_squeezed_pane() {
        // Auto-expand entries carry no from_expand flag, so a lone one is undone like a
        // collapse: the pane on record gets the focus.
        let server = server_with("a", right(0.9, pane("a"), pane("b")));
        write_state(
            &server,
            r#"[{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.5}]"#,
        );
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Restored {
                splits: 1,
                focused_pane_id: "b".into()
            }
        );
    }

    #[test]
    fn restore_applies_each_path_once_in_file_order() {
        let server = server_with("a", right(0.9, pane("a"), pane("b")));
        write_state(
            &server,
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.3, "from_expand": false}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.7, "from_expand": false}]"#
            ),
        );
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        // Two valid entries, one path: one call, and with two entries the focus stays put.
        assert_eq!(
            outcome,
            CollapseOutcome::Restored {
                splits: 1,
                focused_pane_id: "a".into()
            }
        );
        assert_eq!(
            calls(&server),
            vec![ratio_call(json!([]), 0.3, "a"), focus_call("a")]
        );
    }

    #[test]
    fn stale_entries_are_dropped_and_the_pane_is_collapsed_instead() {
        // The entry says pane z was collapsed on the second side; z is gone.
        let server = server_with("a", right(0.5, pane("a"), pane("b")));
        write_state(
            &server,
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "z", "panes": ["z"], "path": [], "side": true, "ratio": 0.3, "from_expand": false}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [true, true], "side": false, "ratio": 0.3, "from_expand": false}]"#
            ),
        );
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Collapsed {
                pane_id: "a".into(),
                focused_pane_id: "b".into()
            }
        );
        assert_eq!(
            read_state(&server),
            r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.5, "from_expand": false}]"#
        );
    }

    #[test]
    fn an_entry_stays_valid_through_any_pane_it_tracked() {
        // The named pane is gone but another pane of the squeezed side is still there.
        let server = server_with("a", right(0.9, pane("a"), down(0.5, pane("c"), pane("d"))));
        write_state(
            &server,
            r#"[{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b", "c"], "path": [], "side": true, "ratio": 0.6, "from_expand": false}]"#,
        );
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Toggle).unwrap();
        // The collapsed pane no longer exists, so the focus stays with the focused pane.
        assert_eq!(
            outcome,
            CollapseOutcome::Restored {
                splits: 1,
                focused_pane_id: "a".into()
            }
        );
        assert_eq!(
            calls(&server),
            vec![ratio_call(json!([]), 0.6, "a"), focus_call("a")]
        );
    }

    #[test]
    fn toggle_keeps_other_tabs_entries_when_collapsing() {
        let server = server_with("b", right(0.5, pane("a"), pane("b")));
        write_state(
            &server,
            r#"[{"tab_id": "w7:t3", "pane_id": "x", "panes": ["x"], "path": [false], "side": true, "ratio": 0.25, "from_expand": true, "note": "kept"}]"#,
        );
        server.ok(OK, ok()).ok("pane.focus", ok());
        collapse(&server.client(), CollapseMode::Toggle).unwrap();
        assert_eq!(
            read_state(&server),
            concat!(
                r#"[{"tab_id": "w7:t3", "pane_id": "x", "panes": ["x"], "path": [false], "side": true, "ratio": 0.25, "from_expand": true, "note": "kept"}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.5, "from_expand": false}]"#
            )
        );
    }

    #[test]
    fn toggle_needs_a_state_file() {
        let server = server_with("a", right(0.5, pane("a"), pane("b")));
        let client = Client::new(server.socket_path());
        assert!(matches!(
            collapse(&client, CollapseMode::Toggle).unwrap_err(),
            HerdrError::NoStateFile
        ));
        assert!(
            calls(&server).is_empty(),
            "nothing may change before the state file is known"
        );
    }

    #[test]
    fn a_failed_squeeze_leaves_the_record_alone() {
        let server = server_with("a", right(0.5, pane("a"), pane("b")));
        server.fail(OK, "split_not_found", "split path not found");
        let err = collapse(&server.client(), CollapseMode::Toggle).unwrap_err();
        assert_eq!(err.code(), Some("split_not_found"));
        assert_eq!(read_state(&server), "[]");
        assert_eq!(
            calls(&server).len(),
            1,
            "no focus change after a failed squeeze"
        );
    }

    #[test]
    fn a_failed_restore_keeps_the_record_for_another_try() {
        let state = r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.5, "from_expand": false}]"#;
        let server = server_with("b", right(0.1, pane("a"), pane("b")));
        write_state(&server, state);
        server.fail(OK, "split_not_found", "split path not found");
        assert!(collapse(&server.client(), CollapseMode::Toggle).is_err());
        assert_eq!(read_state(&server), state);
    }

    // ---- expand ----------------------------------------------------------------------

    #[test]
    fn expand_grows_the_pane_along_every_split_above_it() {
        // (a | (b / c)) with c focused: the root gives the right side 90%, the inner split
        // gives the bottom 90%.
        let server = server_with("c", right(0.5, pane("a"), down(0.3, pane("b"), pane("c"))));
        server.ok(OK, ok()).ok(OK, ok());
        let outcome = collapse(&server.client(), CollapseMode::Expand).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Expanded {
                pane_id: "c".into(),
                splits: 2
            }
        );
        assert_eq!(
            calls(&server),
            vec![
                ratio_call(json!([]), 0.1, "c"),
                ratio_call(json!([true]), 0.1, "c")
            ]
        );
        assert_eq!(
            read_state(&server),
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.5, "from_expand": true}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [true], "side": false, "ratio": 0.3, "from_expand": true}]"#
            )
        );
        server.assert_finished();
    }

    #[test]
    fn expand_records_every_pane_of_a_squeezed_subtree() {
        let server = server_with("a", right(0.5, pane("a"), down(0.5, pane("b"), pane("c"))));
        server.ok(OK, ok());
        collapse(&server.client(), CollapseMode::Expand).unwrap();
        assert_eq!(calls(&server), vec![ratio_call(json!([]), 0.9, "a")]);
        assert_eq!(
            read_state(&server),
            r#"[{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b", "c"], "path": [], "side": true, "ratio": 0.5, "from_expand": true}]"#
        );
    }

    #[test]
    fn expand_skips_splits_that_are_already_where_it_wants_them() {
        // The root is already 90/10 in favour of the right side; only the inner one moves.
        let server = server_with("c", right(0.1, pane("a"), down(0.5, pane("b"), pane("c"))));
        server.ok(OK, ok());
        let outcome = collapse(&server.client(), CollapseMode::Expand).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Expanded {
                pane_id: "c".into(),
                splits: 2
            }
        );
        assert_eq!(calls(&server), vec![ratio_call(json!([true]), 0.1, "c")]);
        // Both splits are on record all the same.
        assert_eq!(
            read_state(&server),
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.1, "from_expand": true}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [true], "side": false, "ratio": 0.5, "from_expand": true}]"#
            )
        );
    }

    #[test]
    fn expand_keeps_the_ratio_a_collapse_had_recorded() {
        // a was collapsed (remembered ratio 0.4); now a is expanded: the entry flips to the
        // other side but keeps 0.4, and stale entries of the tab are dropped.
        let server = server_with("a", right(0.1, pane("a"), pane("b")));
        write_state(
            &server,
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "gone", "panes": ["gone"], "path": [], "side": true, "ratio": 0.8, "from_expand": false}, "#,
                r#"{"tab_id": "w2:t1", "pane_id": "q", "panes": ["q"], "path": [], "side": false, "ratio": 0.7}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.4, "from_expand": false}]"#
            ),
        );
        server.ok(OK, ok());
        let outcome = collapse(&server.client(), CollapseMode::Expand).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Expanded {
                pane_id: "a".into(),
                splits: 1
            }
        );
        assert_eq!(calls(&server), vec![ratio_call(json!([]), 0.9, "a")]);
        assert_eq!(
            read_state(&server),
            concat!(
                r#"[{"tab_id": "w2:t1", "pane_id": "q", "panes": ["q"], "path": [], "side": false, "ratio": 0.7}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.4, "from_expand": true}]"#
            )
        );
    }

    #[test]
    fn expand_again_restores_what_it_recorded() {
        let server = server_with("c", right(0.1, pane("a"), down(0.1, pane("b"), pane("c"))));
        write_state(
            &server,
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.5, "from_expand": true}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [true], "side": false, "ratio": 0.3, "from_expand": true}]"#
            ),
        );
        server.ok(OK, ok()).ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Expand).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Restored {
                splits: 2,
                focused_pane_id: "c".into()
            }
        );
        assert_eq!(
            calls(&server),
            vec![
                ratio_call(json!([]), 0.5, "c"),
                ratio_call(json!([true]), 0.3, "c"),
                focus_call("c")
            ]
        );
        assert_eq!(read_state(&server), "[]");
    }

    #[test]
    fn expand_again_keeps_the_focus_even_for_a_single_plain_entry() {
        // In expand mode the focused pane always keeps the focus after a restore.
        let server = server_with("a", right(0.9, pane("a"), pane("b")));
        write_state(
            &server,
            r#"[{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.5, "from_expand": false}]"#,
        );
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse(&server.client(), CollapseMode::Expand).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Restored {
                splits: 1,
                focused_pane_id: "a".into()
            }
        );
    }

    #[test]
    fn expand_again_with_nothing_on_record_evens_the_splits() {
        let server = server_with("c", right(0.1, pane("a"), down(0.11, pane("b"), pane("c"))));
        server.ok(OK, ok()).ok(OK, ok());
        let outcome = collapse(&server.client(), CollapseMode::Expand).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Evened {
                pane_id: "c".into(),
                splits: 2
            }
        );
        assert!(outcome.changed());
        assert_eq!(
            calls(&server),
            vec![
                ratio_call(json!([]), 0.5, "c"),
                ratio_call(json!([true]), 0.5, "c")
            ]
        );
        assert_eq!(read_state(&server), "[]");
    }

    #[test]
    fn a_failed_expand_still_records_what_it_changed() {
        let server = server_with("c", right(0.5, pane("a"), down(0.3, pane("b"), pane("c"))));
        server
            .ok(OK, ok())
            .fail(OK, "split_not_found", "split path not found");
        let err = collapse(&server.client(), CollapseMode::Expand).unwrap_err();
        assert_eq!(err.code(), Some("split_not_found"));
        // The root split did change and can be restored; the inner entry is harmless.
        assert!(read_state(&server).starts_with(
            r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.5, "from_expand": true}"#
        ));
    }

    // ---- auto ------------------------------------------------------------------------

    #[test]
    fn auto_does_nothing_when_the_focused_pane_has_room() {
        for (focused, root) in [
            ("a", pane("a")),
            ("a", right(0.5, pane("a"), pane("b"))),
            ("b", right(0.5, pane("a"), pane("b"))),
            ("a", right(0.13, pane("a"), pane("b"))),
            ("b", right(0.87, pane("a"), pane("b"))),
            ("a", right(0.9, pane("a"), pane("b"))),
        ] {
            let server = server_with(focused, root);
            let outcome = collapse(&Client::new(server.socket_path()), CollapseMode::Auto).unwrap();
            assert_eq!(outcome, CollapseOutcome::Unchanged);
            assert!(calls(&server).is_empty());
        }
    }

    #[test]
    fn auto_flips_a_collapsed_split_around_the_focused_pane() {
        // a is the 10% sliver and now has the focus.
        let server = server_with("a", right(0.1, pane("a"), pane("b")));
        server.ok(OK, ok());
        let outcome = collapse(&server.client(), CollapseMode::Auto).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::AutoExpanded {
                pane_id: "a".into(),
                splits: 1
            }
        );
        assert_eq!(calls(&server), vec![ratio_call(json!([]), 0.9, "a")]);
        assert_eq!(
            read_state(&server),
            r#"[{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.5}]"#
        );
        server.assert_finished();
    }

    #[test]
    fn auto_treats_everything_up_to_twelve_percent_as_collapsed() {
        for (ratio, focused, wanted) in [
            (0.12, "a", 0.9),
            (0.115, "a", 0.9),
            (0.88, "b", 0.1),
            (0.9, "b", 0.1),
        ] {
            let server = server_with(focused, right(ratio, pane("a"), pane("b")));
            server.ok(OK, ok());
            let outcome = collapse(&server.client(), CollapseMode::Auto).unwrap();
            assert!(
                matches!(outcome, CollapseOutcome::AutoExpanded { splits: 1, .. }),
                "{ratio} {focused}"
            );
            assert_eq!(calls(&server), vec![ratio_call(json!([]), wanted, focused)]);
        }
    }

    #[test]
    fn auto_flips_every_collapsed_split_above_the_pane() {
        // ((a | b) / c): c takes 90% and a is the sliver of the top row. Focus on a.
        let server = server_with("a", down(0.1, right(0.1, pane("a"), pane("b")), pane("c")));
        server.ok(OK, ok()).ok(OK, ok());
        let outcome = collapse(&server.client(), CollapseMode::Auto).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::AutoExpanded {
                pane_id: "a".into(),
                splits: 2
            }
        );
        assert_eq!(
            calls(&server),
            vec![
                ratio_call(json!([]), 0.9, "a"),
                ratio_call(json!([false]), 0.9, "a")
            ]
        );
        assert_eq!(
            read_state(&server),
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "c", "panes": ["c"], "path": [], "side": true, "ratio": 0.5}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [false], "side": true, "ratio": 0.5}]"#
            )
        );
    }

    #[test]
    fn auto_only_flips_the_splits_that_are_collapsed() {
        // The root is even; only the inner split squeezes c.
        let server = server_with("c", right(0.5, pane("a"), down(0.9, pane("b"), pane("c"))));
        server.ok(OK, ok());
        let outcome = collapse(&server.client(), CollapseMode::Auto).unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::AutoExpanded {
                pane_id: "c".into(),
                splits: 1
            }
        );
        assert_eq!(calls(&server), vec![ratio_call(json!([true]), 0.1, "c")]);
    }

    #[test]
    fn auto_updates_the_collapse_record_and_keeps_its_ratio() {
        // a was collapsed by toggle (remembered 0.42); focus comes back to a.
        let server = server_with("a", right(0.1, pane("a"), pane("b")));
        write_state(
            &server,
            r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.42, "from_expand": false}]"#,
        );
        server.ok(OK, ok());
        collapse(&server.client(), CollapseMode::Auto).unwrap();
        assert_eq!(
            read_state(&server),
            r#"[{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.42, "from_expand": false}]"#
        );
    }

    #[test]
    fn auto_updates_the_newest_matching_entry_and_ignores_other_tabs() {
        let server = server_with("a", right(0.1, pane("a"), pane("b")));
        write_state(
            &server,
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.3}, "#,
                r#"{"tab_id": "w2:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.6}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.7}]"#
            ),
        );
        server.ok(OK, ok());
        collapse(&server.client(), CollapseMode::Auto).unwrap();
        assert_eq!(
            read_state(&server),
            concat!(
                r#"[{"tab_id": "w1:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.3}, "#,
                r#"{"tab_id": "w2:t1", "pane_id": "a", "panes": ["a"], "path": [], "side": false, "ratio": 0.6}, "#,
                r#"{"tab_id": "w1:t1", "pane_id": "b", "panes": ["b"], "path": [], "side": true, "ratio": 0.7}]"#
            )
        );
    }

    #[test]
    fn auto_needs_a_state_file_only_when_it_has_something_to_flip() {
        let server = server_with("a", right(0.1, pane("a"), pane("b")));
        let err = collapse(&Client::new(server.socket_path()), CollapseMode::Auto).unwrap_err();
        assert!(matches!(err, HerdrError::NoStateFile));
        assert!(
            calls(&server).is_empty(),
            "nothing is flipped that could not be recorded"
        );
    }

    #[test]
    fn a_failed_flip_records_the_flips_before_it() {
        let server = server_with("a", down(0.1, right(0.1, pane("a"), pane("b")), pane("c")));
        server
            .ok(OK, ok())
            .fail(OK, "split_not_found", "split path not found");
        let err = collapse(&server.client(), CollapseMode::Auto).unwrap_err();
        assert_eq!(err.code(), Some("split_not_found"));
        assert_eq!(
            read_state(&server),
            r#"[{"tab_id": "w1:t1", "pane_id": "c", "panes": ["c"], "path": [], "side": true, "ratio": 0.5}]"#
        );
    }

    #[test]
    fn a_first_flip_that_fails_writes_nothing() {
        let server = server_with("a", right(0.1, pane("a"), pane("b")));
        server.fail(OK, "split_not_found", "split path not found");
        assert!(collapse(&server.client(), CollapseMode::Auto).is_err());
        assert!(!server.state_file().exists());
    }

    // ---- targets and empty sessions ---------------------------------------------------

    #[test]
    fn an_empty_session_is_unchanged_in_every_mode() {
        for mode in [
            CollapseMode::Toggle,
            CollapseMode::Expand,
            CollapseMode::Auto,
        ] {
            for code in ["layout_not_found", "pane_not_found"] {
                let server = MockServer::start();
                server.fail("layout.export", code, "layout target not found");
                assert_eq!(
                    collapse(&server.client(), mode).unwrap(),
                    CollapseOutcome::Unchanged
                );
                assert_eq!(server.requests()[0].params, json!({}));
            }
        }
    }

    #[test]
    fn other_export_failures_are_errors() {
        let server = MockServer::start();
        server.fail("layout.export", "invalid_request", "nope");
        assert_eq!(
            collapse(&server.client(), CollapseMode::Toggle)
                .unwrap_err()
                .code(),
            Some("invalid_request")
        );
    }

    #[test]
    fn collapse_pane_acts_on_the_named_pane_instead_of_the_focused_one() {
        // b is named; the tab's focused pane is a.
        let server = server_with("a", right(0.5, pane("a"), pane("b")));
        server.ok(OK, ok()).ok("pane.focus", ok());
        let outcome = collapse_pane(&server.client(), CollapseMode::Toggle, "b").unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Collapsed {
                pane_id: "b".into(),
                focused_pane_id: "a".into()
            }
        );
        assert_eq!(server.requests()[0].params, json!({"pane_id": "b"}));
        assert_eq!(
            calls(&server),
            vec![ratio_call(json!([]), 0.9, "b"), focus_call("a")]
        );
    }

    #[test]
    fn collapse_pane_expand_uses_the_named_pane() {
        let server = server_with("a", right(0.5, pane("a"), pane("b")));
        server.ok(OK, ok());
        let outcome = collapse_pane(&server.client(), CollapseMode::Expand, "b").unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::Expanded {
                pane_id: "b".into(),
                splits: 1
            }
        );
        assert_eq!(calls(&server), vec![ratio_call(json!([]), 0.1, "b")]);
    }

    #[test]
    fn auto_for_a_pane_that_is_not_focused_does_nothing() {
        // The event named b, but a is the focused pane of the tab: the event is stale.
        let server = server_with("a", right(0.9, pane("a"), pane("b")));
        let outcome = collapse_pane(&server.client(), CollapseMode::Auto, "b").unwrap();
        assert_eq!(outcome, CollapseOutcome::Unchanged);
        assert!(calls(&server).is_empty());
    }

    #[test]
    fn auto_for_the_focused_pane_flips_its_split() {
        let server = server_with("b", right(0.9, pane("a"), pane("b")));
        server.ok(OK, ok());
        let outcome = collapse_pane(&server.client(), CollapseMode::Auto, "b").unwrap();
        assert_eq!(
            outcome,
            CollapseOutcome::AutoExpanded {
                pane_id: "b".into(),
                splits: 1
            }
        );
        assert_eq!(calls(&server), vec![ratio_call(json!([]), 0.1, "b")]);
    }

    #[test]
    fn an_unknown_named_pane_is_an_error() {
        let server = MockServer::start();
        server.fail("layout.export", "pane_not_found", "pane w9:p9 not found");
        let err = collapse_pane(&server.client(), CollapseMode::Toggle, "w9:p9").unwrap_err();
        assert_eq!(err.code(), Some("pane_not_found"));
    }

    #[test]
    fn a_named_pane_missing_from_its_own_layout_counts_as_a_single_pane() {
        // herdr answered with a tab that does not contain the pane: treat it like the script
        // does, as nothing to collapse.
        let server = server_with("a", right(0.5, pane("a"), pane("b")));
        let outcome = collapse_pane(&server.client(), CollapseMode::Toggle, "zz").unwrap();
        assert_eq!(outcome, CollapseOutcome::SinglePane);
    }

    #[test]
    fn collapse_then_restore_round_trips_through_the_state_file() {
        // First call collapses b; herdr then reports the squeezed layout with focus on a.
        let server = MockServer::start();
        server.ok(
            "layout.export",
            export_result("b", false, right(0.35, pane("a"), pane("b"))),
        );
        server.ok(OK, ok()).ok("pane.focus", ok());
        server.ok(
            "layout.export",
            export_result("a", false, right(0.9, pane("a"), pane("b"))),
        );
        server.ok(OK, ok()).ok("pane.focus", ok());
        let client = server.client();
        assert_eq!(
            collapse(&client, CollapseMode::Toggle).unwrap(),
            CollapseOutcome::Collapsed {
                pane_id: "b".into(),
                focused_pane_id: "a".into()
            }
        );
        assert_eq!(
            collapse(&client, CollapseMode::Toggle).unwrap(),
            CollapseOutcome::Restored {
                splits: 1,
                focused_pane_id: "b".into()
            }
        );
        let requests = server.requests();
        assert_eq!(
            requests[4].params,
            json!({"path": [], "ratio": 0.35, "pane_id": "a"})
        );
        assert_eq!(requests[5].params, json!({"pane_id": "b"}));
        assert_eq!(read_state(&server), "[]");
        server.assert_finished();
    }

    #[test]
    fn the_thresholds_are_the_scripts() {
        assert_eq!(MIN_RATIO, 0.1);
        assert_eq!(SLIVER_THRESHOLD, 0.1 + 0.02);
        assert_eq!(1.0 - MIN_RATIO, 0.9);
    }
}
