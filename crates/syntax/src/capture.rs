//! Highlight capture names.
//!
//! Grammars name what they capture in three overlapping dialects: tree-sitter's own
//! (`function.method`, `escape`), the older nvim-treesitter one (`text.title`, `field`,
//! `conditional`) and the newer one (`markup.heading`, `variable.member`). A theme should not
//! have to know which grammar it is looking at, so every name is rewritten once, when its
//! grammar is first loaded, into the vocabulary Zed themes use, and then interned as a small
//! [`CaptureId`] that a span can carry cheaply.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use bakedit_core::{SyntaxStyle, Theme};

/// The most distinct capture names the interner will hand out. The grammars this crate
/// bundles produce a few hundred between them.
const MAX_CAPTURES: usize = 2048;

/// Names by id. Written once each under [`TABLE`]'s lock, read without one.
static NAMES: [OnceLock<&'static str>; MAX_CAPTURES] = [const { OnceLock::new() }; MAX_CAPTURES];

/// Name to id, for interning.
static TABLE: Mutex<Option<HashMap<&'static str, u16>>> = Mutex::new(None);

/// A normalised highlight capture name, such as `keyword` or `function.method`.
///
/// Ids are small, dense and stable for the life of the process, but not across runs: store
/// names, not ids. Resolve one with [`capture_name`] or [`CaptureId::name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CaptureId(u16);

impl CaptureId {
    /// The id as an index, for tables keyed by capture (ids count up from 0).
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// The capture's name; the same as [`capture_name`].
    pub fn name(self) -> &'static str {
        capture_name(self)
    }

    /// The part of the name before the first dot: `function` for `function.method`.
    pub fn root(self) -> &'static str {
        let name = self.name();
        name.split('.').next().unwrap_or(name)
    }
}

/// The name of a capture, such as `function.method`. Pass it to
/// `bakedit_core::Theme::syntax`, which falls back along the dots.
pub fn capture_name(id: CaptureId) -> &'static str {
    NAMES
        .get(id.index())
        .and_then(|slot| slot.get())
        .copied()
        .unwrap_or("")
}

/// The id of an already known capture name, or `None` if no loaded grammar produces it.
/// The name must be in its normalised form (`title`, not `text.title`).
pub fn capture_id(name: &str) -> Option<CaptureId> {
    let table = TABLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    table.as_ref()?.get(name).map(|&id| CaptureId(id))
}

/// How many capture ids exist so far. Ids are `0..capture_count()`; the number grows as
/// grammars are loaded.
pub fn capture_count() -> usize {
    let table = TABLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    table.as_ref().map_or(0, HashMap::len)
}

/// Intern a normalised capture name. `None` only if the table is full, which the bundled
/// grammars cannot cause.
pub(crate) fn intern(name: &str) -> Option<CaptureId> {
    let mut guard = TABLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let table = guard.get_or_insert_with(HashMap::new);
    if let Some(&id) = table.get(name) {
        return Some(CaptureId(id));
    }
    let index = table.len();
    let slot = NAMES.get(index)?;
    // Capture names live as long as the process: there are few of them and spans refer to
    // them from every thread.
    let name: &'static str = Box::leak(name.to_owned().into_boxed_str());
    let stored = *slot.get_or_init(|| name);
    table.insert(stored, index as u16);
    Some(CaptureId(index as u16))
}

/// Captures that carry no colour of their own. nvim-treesitter uses them to steer spell
/// checking and concealment, and tree-sitter-make uses `clean` as a helper for a predicate.
const NOT_HIGHLIGHTS: &[&str] = &["spell", "nospell", "conceal", "clean"];

/// Whole-segment prefixes that are renamed, longest first where two overlap. What follows
/// the prefix is kept, so `markup.heading.1` becomes `title.1`.
const RENAMES: &[(&str, &str)] = &[
    // Prose, old nvim-treesitter names.
    ("text.title", "title"),
    ("text.uri", "link_uri"),
    ("text.emphasis", "emphasis"),
    ("text.strong", "emphasis.strong"),
    ("text.literal", "string"),
    ("text.reference", "link_text"),
    // Prose, new nvim-treesitter names.
    ("markup.heading", "title"),
    ("markup.link.url", "link_uri"),
    ("markup.link", "link_text"),
    ("markup.italic", "emphasis"),
    ("markup.bold", "emphasis.strong"),
    ("markup.raw", "string"),
    // Code, old nvim-treesitter names.
    ("float", "number.float"),
    ("field", "property"),
    ("parameter", "variable.parameter"),
    ("method", "function.method"),
    ("include", "keyword.import"),
    ("conditional", "keyword.conditional"),
    ("repeat", "keyword.repeat"),
    ("exception", "keyword.exception"),
    ("storageclass", "keyword.modifier"),
    ("define", "keyword.directive.define"),
    ("preproc", "keyword.directive"),
    ("namespace", "type.namespace"),
    ("character", "string.character"),
    // Not a character literal: a wildcard `_`, the flags of a regex.
    ("character.special", "punctuation.special"),
    ("type.qualifier", "keyword.modifier"),
    // Code, new nvim-treesitter names.
    ("module", "type.namespace"),
    ("variable.member", "property"),
    // tree-sitter's own names.
    ("escape", "string.escape"),
    ("delimiter", "punctuation.delimiter"),
    // Zed colours JSON keys as properties, not as strings.
    ("string.special.key", "property.json_key"),
];

/// Rewrite a grammar's capture name into the theme vocabulary.
///
/// `None` means the capture is not a highlight at all: private captures (`_name`), the
/// bookkeeping ones for injections and locals, and the spelling and concealment markers.
pub(crate) fn normalize(raw: &str) -> Option<String> {
    let root = raw.split('.').next().unwrap_or(raw);
    if raw.is_empty()
        || raw.starts_with('_')
        || NOT_HIGHLIGHTS.contains(&root)
        || root == "injection"
        || root == "local"
    {
        return None;
    }
    let renamed = RENAMES
        .iter()
        .filter(|(prefix, _)| has_segment_prefix(raw, prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(prefix, replacement)| format!("{replacement}{}", &raw[prefix.len()..]));
    Some(renamed.unwrap_or_else(|| raw.to_owned()))
}

/// Whether `name` is `prefix` or starts with `prefix` followed by a dot.
fn has_segment_prefix(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// Remembers how a theme draws each capture, so the draw loop does one index instead of a
/// string lookup per span.
///
/// Build one per theme and ask it with the same theme each time; make a new one when the
/// theme changes. Capture ids that appear later (a grammar loads on first use) are resolved
/// when first asked for.
#[derive(Debug, Clone, Default)]
pub struct StyleCache {
    styles: Vec<Option<Option<SyntaxStyle>>>,
}

impl StyleCache {
    /// An empty cache.
    pub fn new() -> StyleCache {
        StyleCache::default()
    }

    /// How `theme` draws `capture`: `None` when the theme has no style for it or any of its
    /// dotted parents, in which case the text keeps the editor's foreground colour.
    pub fn style(&mut self, theme: &Theme, capture: CaptureId) -> Option<SyntaxStyle> {
        let index = capture.index();
        if index >= self.styles.len() {
            self.styles.resize(index + 1, None);
        }
        *self.styles[index].get_or_insert_with(|| theme.syntax(capture_name(capture)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_foreign_roots_and_keeps_the_detail() {
        let cases = [
            ("text.title", "title"),
            ("markup.heading", "title"),
            ("markup.heading.2", "title.2"),
            ("text.uri", "link_uri"),
            ("markup.link.url", "link_uri"),
            ("markup.link", "link_text"),
            ("markup.link.label", "link_text.label"),
            ("text.reference", "link_text"),
            ("text.emphasis", "emphasis"),
            ("markup.italic", "emphasis"),
            ("text.strong", "emphasis.strong"),
            ("markup.bold", "emphasis.strong"),
            ("text.literal", "string"),
            ("markup.raw.block", "string.block"),
            ("float", "number.float"),
            ("field", "property"),
            ("variable.member", "property"),
            ("parameter", "variable.parameter"),
            ("method", "function.method"),
            ("method.call", "function.method.call"),
            ("include", "keyword.import"),
            ("conditional", "keyword.conditional"),
            ("repeat", "keyword.repeat"),
            ("exception", "keyword.exception"),
            ("storageclass", "keyword.modifier"),
            ("define", "keyword.directive.define"),
            ("preproc", "keyword.directive"),
            ("namespace", "type.namespace"),
            ("module", "type.namespace"),
            ("module.builtin", "type.namespace.builtin"),
            ("escape", "string.escape"),
            ("character", "string.character"),
            ("character.special", "punctuation.special"),
            ("character.printf", "string.character.printf"),
            ("delimiter", "punctuation.delimiter"),
            ("type.qualifier", "keyword.modifier"),
            ("string.special.key", "property.json_key"),
        ];
        for (raw, expected) in cases {
            assert_eq!(normalize(raw).as_deref(), Some(expected), "{raw}");
        }
    }

    #[test]
    fn keeps_names_already_in_the_vocabulary() {
        for name in [
            "keyword",
            "keyword.conditional.ternary",
            "string",
            "string.escape",
            "string.special.symbol",
            "comment.documentation",
            "function.method",
            "function.builtin",
            "function.macro",
            "type.builtin",
            "constant.builtin",
            "variable.builtin",
            "variable.parameter",
            "punctuation.bracket",
            "punctuation.special",
            "label",
            "embedded",
            "none",
            "tag",
            "attribute",
            "constructor",
            "operator",
            "number",
            "boolean",
            "property",
        ] {
            assert_eq!(normalize(name).as_deref(), Some(name));
        }
    }

    #[test]
    fn a_prefix_only_matches_whole_segments() {
        assert_eq!(normalize("fields").as_deref(), Some("fields"));
        assert_eq!(normalize("methodology.x").as_deref(), Some("methodology.x"));
        assert_eq!(normalize("text.titles").as_deref(), Some("text.titles"));
        assert_eq!(normalize("text.note").as_deref(), Some("text.note"));
    }

    #[test]
    fn bookkeeping_captures_are_not_highlights() {
        for name in [
            "",
            "_name",
            "_function",
            "spell",
            "nospell",
            "conceal",
            "conceal.lines",
            "clean",
            "injection.content",
            "injection.language",
            "local.scope",
            "local.definition",
            "local.definition.function",
            "local.reference",
        ] {
            assert_eq!(normalize(name), None, "{name:?}");
        }
    }

    #[test]
    fn interning_is_stable_and_reversible() {
        let a = intern("test.capture.alpha").unwrap();
        let b = intern("test.capture.beta").unwrap();
        assert_ne!(a, b);
        assert_eq!(intern("test.capture.alpha"), Some(a));
        assert_eq!(capture_name(a), "test.capture.alpha");
        assert_eq!(a.name(), "test.capture.alpha");
        assert_eq!(a.root(), "test");
        assert_eq!(capture_id("test.capture.beta"), Some(b));
        assert_eq!(capture_id("test.capture.never.interned"), None);
        assert!(capture_count() > a.index().max(b.index()));
    }

    #[test]
    fn interning_from_many_threads_agrees() {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..50)
                        .map(|i| intern(&format!("test.threads.{i}")).unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let results: Vec<Vec<CaptureId>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for other in &results[1..] {
            assert_eq!(other, &results[0]);
        }
        for (i, id) in results[0].iter().enumerate() {
            assert_eq!(capture_name(*id), format!("test.threads.{i}"));
        }
    }

    #[test]
    fn style_cache_follows_the_theme_fallback() {
        let theme = Theme::builtin();
        let mut cache = StyleCache::new();
        let method = intern("function.method").unwrap();
        let unstyled = intern("test.capture.unstyled").unwrap();
        assert_eq!(cache.style(&theme, method), theme.syntax("function"));
        assert!(cache.style(&theme, method).is_some());
        assert_eq!(cache.style(&theme, unstyled), None);
        // Asked again, the remembered answer is the same.
        assert_eq!(cache.style(&theme, method), theme.syntax("function.method"));
    }
}
