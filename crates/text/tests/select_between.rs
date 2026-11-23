//! The select-between port, checked against answers recorded from the original
//! `zed-select-between` script.
//!
//! `fixtures/select_between.json` holds random texts with, for every delimiter kind, the
//! pairs the script's `find_bracket_pairs` / `find_quote_pairs` return and what its
//! `run_worker` decides for a set of cursors and selections.

use std::ops::Range;

use bakedit_text::pairs::{find_pairs, select_between, BetweenTarget, PairKind};
use bakedit_text::ropey::Rope;
use serde_json::Value;

fn as_usize(value: &Value) -> usize {
    value.as_u64().expect("a non-negative number") as usize
}

fn check_fixture(json: &str) -> usize {
    let cases: Value = serde_json::from_str(json).expect("fixture is valid JSON");
    let mut checked = 0;
    for case in cases.as_array().expect("an array of cases") {
        let text = case["text"].as_str().expect("text");
        let rope = Rope::from_str(text);
        for kind in PairKind::ALL {
            let expected = &case["kinds"][kind.name()];
            let (pairs, first_open) = find_pairs(&rope, kind);

            let got: Vec<(usize, usize, usize)> = pairs
                .iter()
                .map(|p| (p.open, p.delim_len, p.close))
                .collect();
            let want: Vec<(usize, usize, usize)> = expected["pairs"]
                .as_array()
                .expect("pairs")
                .iter()
                .map(|p| (as_usize(&p[0]), as_usize(&p[1]), as_usize(&p[2])))
                .collect();
            assert_eq!(got, want, "pairs of {} in {text:?}", kind.name());
            assert_eq!(
                first_open,
                expected["first_open"].as_u64().map(|n| n as usize),
                "first opener of {} in {text:?}",
                kind.name()
            );

            for decision in expected["decisions"].as_array().expect("decisions") {
                let cursor = as_usize(&decision[0]);
                let selected: Range<usize> = as_usize(&decision[1])..as_usize(&decision[2]);
                let outcome = decision[3].as_array().expect("outcome");
                let want = match outcome[0].as_str() {
                    Some("select") => {
                        BetweenTarget::Select(as_usize(&outcome[1])..as_usize(&outcome[2]))
                    }
                    Some("cursor") => BetweenTarget::Cursor(as_usize(&outcome[1])),
                    Some("none") => BetweenTarget::Nothing,
                    other => panic!("unknown outcome {other:?}"),
                };
                let got = select_between(&rope, kind, cursor, selected.clone());
                assert_eq!(
                    got,
                    want,
                    "{} at {cursor} with {selected:?} selected in {text:?}",
                    kind.name()
                );
                checked += 1;
            }
        }
    }
    checked
}

#[test]
fn matches_the_original_script() {
    let checked = check_fixture(include_str!("fixtures/select_between.json"));
    assert!(checked > 700, "only {checked} decisions were checked");
}

/// Run against a larger fixture produced with the same generator, when one is supplied.
#[test]
fn matches_the_original_script_on_an_extra_fixture() {
    if let Ok(path) = std::env::var("BAKEDIT_SELECT_BETWEEN_FIXTURE") {
        let json = std::fs::read_to_string(&path).expect("the extra fixture is readable");
        let checked = check_fixture(&json);
        assert!(checked > 0);
    }
}
