//! Fuzzy filtering for the lists whose rows the app already holds in memory: the command
//! palette, the tab switcher, the outline, branches. (The file finder has its own index in
//! `bakedit-project`.)

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Index of the row in the input.
    pub index: usize,
    pub score: u32,
    /// Char positions in the row's text that matched, ascending.
    pub positions: Vec<usize>,
}

/// The rows that match `query`, best first; ties keep their original order. An empty query
/// matches everything in the original order. Matching is case-insensitive unless the query
/// has an upper-case letter, and space-separated words must all match.
pub fn filter<'a>(rows: impl IntoIterator<Item = &'a str>, query: &str) -> Vec<FuzzyMatch> {
    let query = query.trim();
    if query.is_empty() {
        return rows
            .into_iter()
            .enumerate()
            .map(|(index, _)| FuzzyMatch { index, score: 0, positions: Vec::new() })
            .collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buffer = Vec::new();
    let mut indices = Vec::new();
    let mut matches: Vec<FuzzyMatch> = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        indices.clear();
        let haystack = Utf32Str::new(row, &mut buffer);
        if let Some(score) = pattern.indices(haystack, &mut matcher, &mut indices) {
            let mut positions: Vec<usize> = indices.iter().map(|&i| i as usize).collect();
            positions.sort_unstable();
            positions.dedup();
            matches.push(FuzzyMatch { index, score, positions });
        }
    }
    matches.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: [&str; 5] = [
        "workspace: save",
        "workspace: save all",
        "editor: select all",
        "git: stage all",
        "file finder: open a file",
    ];

    fn labels(query: &str) -> Vec<&'static str> {
        filter(ROWS, query).into_iter().map(|m| ROWS[m.index]).collect()
    }

    #[test]
    fn an_empty_query_keeps_everything_in_order() {
        assert_eq!(labels(""), ROWS.to_vec());
        assert_eq!(labels("   "), ROWS.to_vec());
        assert!(filter(ROWS, "").iter().all(|m| m.positions.is_empty()));
    }

    #[test]
    fn filters_and_ranks() {
        let found = labels("save");
        assert_eq!(found, vec!["workspace: save", "workspace: save all"]);
        assert_eq!(labels("zzz"), Vec::<&str>::new());
        // Letters scattered across words still match.
        assert!(labels("wsa").contains(&"workspace: save all"));
        // Every word of the query must match.
        assert_eq!(labels("git all"), vec!["git: stage all"]);
    }

    #[test]
    fn positions_point_at_the_matched_characters() {
        let matches = filter(["workspace: save"], "save");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].positions, vec![11, 12, 13, 14]);
        // Positions are char positions, not bytes.
        let matches = filter(["café: save"], "save");
        assert_eq!(matches[0].positions, vec![6, 7, 8, 9]);
    }

    #[test]
    fn smart_case() {
        assert_eq!(filter(["Save", "save"], "save").len(), 2);
        let upper = filter(["Save", "save"], "Save");
        assert_eq!(upper.len(), 1);
        assert_eq!(upper[0].index, 0);
    }
}
