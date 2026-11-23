//! The highlighter: text in, coloured spans out.
//!
//! A document is highlighted as a stack of layers. The first layer is the whole text in the
//! file's own language. Its injections query finds the parts written in another language
//! (the code in a Markdown fence, the script in an HTML page, the arguments of a Rust macro)
//! and each becomes a layer of its own, parsed over just those byte ranges, which may in
//! turn inject further layers. Every layer runs its highlights query and contributes spans;
//! at the end the spans of all layers are flattened so that each byte has one capture, the
//! innermost.
//!
//! This is the job tree-sitter-highlight does, done here directly on tree-sitter's query API
//! for three reasons. Its event stream does not nest properly across layers (a layer's first
//! token and the node that contains the layer start at the same byte, and the token is
//! reported first), so "innermost wins" cannot be recovered from it. It cuts every child out
//! of an injected node, which empties the injections tree-sitter-md's queries make. And it
//! applies one precedence rule to every query, while the queries grammars ship were written
//! for two.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::ops::{ControlFlow, Range as ByteRange};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tree_sitter::{
    Node, ParseOptions, ParseState, Parser, QueryCursor, QueryCursorOptions, QueryCursorState,
    QueryMatch, Range, StreamingIterator, Tree,
};

use crate::capture::CaptureId;
use crate::error::SyntaxError;
use crate::grammar::{
    self, FixedLanguage, Grammar, GrammarSpec, InjectedChildren, LocalRole, Precedence, Predicate,
};
use crate::language::Language;
use crate::registry;

/// How deep injections may nest: a Markdown fence holding HTML holding a script is three.
/// Rust macros inside macros are the only thing that goes deeper in practice, and each level
/// re-parses the text below it, so the limit also bounds the work.
const MAX_INJECTION_DEPTH: u16 = 10;

/// The most layers one document may have. A long Markdown file has one per paragraph.
const MAX_LAYERS: usize = 200_000;

/// The largest span buffer a highlighter keeps between calls (a few megabytes; a file of
/// tens of thousands of lines). One huge file should not pin its memory for good.
const KEEP_COLLECTED: usize = 1 << 18;

// Guards against a query that takes far longer than the parse it follows.
//
// tree-sitter matches a pattern in time linear in the tree unless the pattern has to keep a
// match open across a long subtree. `(call function: (field object: (_) name: (_) @method))`
// starts a match at every call in `a.b().c().d()` and each stays open until the walk comes
// back up to its name, so a chain of n calls costs n squared. Real chains are tens long. A
// degenerate one (generated or minified code, or a file read as the wrong language) is
// thousands: tree-sitter-nix's injections take two minutes on 20,000 chained applications
// that parse in a third of a second.

/// How far below the node a query is run from a match may start;
/// `QueryCursor::set_max_start_depth`. Deeper code is not skipped: the query is run again
/// from each node at that depth (see [`band_roots`]), so a tree is covered in bands this
/// tall. Within a band at most this many matches are open at once, which makes the cost of
/// a chain this number times its length instead of its length squared. Ordinary code is a
/// few dozen levels deep and is one band.
pub(crate) const BAND_DEPTH: u32 = 256;

/// How many matches that have captured something may be open at once; past this the oldest
/// are dropped. Neovim and Helix use the same number.
pub(crate) const MATCH_LIMIT: u32 = 256;

/// The last resort, for a slow query the two limits above do not catch: a query may run for
/// this many times as long as its layer took to parse, and at least [`MIN_QUERY_TIME`],
/// before it is cut short. A layer cut short keeps the injections and spans found so far.
/// A healthy query finishes in about the time of the parse.
const QUERY_TIME_FACTOR: u32 = 20;

/// See [`QUERY_TIME_FACTOR`]. Long enough that a busy machine does not trip it.
const MIN_QUERY_TIME: Duration = Duration::from_secs(2);

/// How long a layer's parse may take: this, plus [`PARSE_TIME_PER_BYTE`] for each byte of
/// the layer. Parsers handle megabytes a second; a few grammars have inputs they are
/// thousands of times slower on (tree-sitter-bash takes seconds over 40 kB of chained method
/// calls). A layer that runs out of time is skipped, so one bad fence in a Markdown file
/// costs the fence its colours and nothing else.
const MIN_PARSE_TIME: Duration = Duration::from_secs(2);

/// See [`MIN_PARSE_TIME`]: two seconds a megabyte.
const PARSE_TIME_PER_BYTE: Duration = Duration::from_nanos(2_000);

/// A run of text drawn with one capture.
///
/// [`Highlighter::highlight`] returns these sorted by `start`, never overlapping, never
/// empty, and with both ends on character boundaries. Text between spans has no highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HighlightSpan {
    /// Byte offset of the first byte.
    pub start: usize,
    /// Byte offset one past the last byte.
    pub end: usize,
    /// What the text is; see [`capture_name`](crate::capture_name).
    pub capture: CaptureId,
}

impl HighlightSpan {
    /// The span as a byte range.
    pub fn range(&self) -> ByteRange<usize> {
        self.start..self.end
    }
}

/// The spans that touch `range`, from a sorted, non-overlapping list such as
/// [`Highlighter::highlight`] returns. The first and last may stick out of the range. Two
/// binary searches; use it to draw part of a text without building a
/// [`LineHighlights`](crate::LineHighlights).
pub fn spans_in(spans: &[HighlightSpan], range: ByteRange<usize>) -> &[HighlightSpan] {
    let first = spans.partition_point(|span| span.end <= range.start);
    let count = spans[first..].partition_point(|span| span.start < range.end);
    &spans[first..first + count]
}

/// Highlights text. Reusable: keep one per thread and call it for every document, so the
/// parser and its buffers are allocated once.
pub struct Highlighter {
    parser: Parser,
    cursor: QueryCursor,
    /// Every layer's spans, before flattening. Kept between calls for its allocation.
    collected: Vec<Collected>,
    /// The captures that start at the byte being processed.
    run: Vec<RunCapture>,
}

impl Default for Highlighter {
    fn default() -> Highlighter {
        Highlighter::new()
    }
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter").finish_non_exhaustive()
    }
}

/// One capture of one layer, before flattening.
#[derive(Debug, Clone, Copy)]
struct Collected {
    start: u32,
    end: u32,
    /// The injection depth of the layer it came from.
    depth: u16,
    capture: CaptureId,
}

/// One capture of a highlights or locals pattern, as the query reports it.
#[derive(Debug, Clone, Copy)]
struct RunCapture {
    start: usize,
    end: usize,
    pattern: usize,
    capture: u32,
    /// For a definition: where its value ends. The name is not visible before that.
    value_end: usize,
}

/// One document or injected fragment, in one language.
struct Layer {
    grammar: &'static Grammar,
    /// The byte ranges the layer covers, in order; empty means the whole text.
    ranges: Vec<Range>,
    depth: u16,
    /// The language of the layer that injected this one.
    parent: Option<&'static GrammarSpec>,
}

impl Highlighter {
    /// A highlighter with nothing loaded. Grammars load the first time a text needs them.
    pub fn new() -> Highlighter {
        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(MATCH_LIMIT);
        cursor.set_max_start_depth(Some(BAND_DEPTH));
        Highlighter {
            parser: Parser::new(),
            cursor,
            collected: Vec::new(),
            run: Vec::new(),
        }
    }

    /// Highlight `text` as `language`.
    ///
    /// Returns the spans sorted, non-overlapping and non-empty, each byte carrying the
    /// innermost capture that covers it. A language without a grammar gives no spans.
    /// Syntax errors in `text` are not errors here: tree-sitter recovers and the rest of the
    /// text is highlighted as usual.
    ///
    /// `cancel` is checked while parsing and while running queries; once it is `true` the
    /// call stops and returns [`SyntaxError::Cancelled`]. The highlighter is ready for the
    /// next call either way.
    ///
    /// The only other errors are a text longer than 4 GiB and a bundled grammar that fails
    /// to load, which would be a bug in this crate.
    ///
    /// The time taken is proportional to the length of the text. To keep that true of
    /// degenerate input, a query that runs for many times as long as the parse did is cut
    /// short where it is, and a parse that takes seconds longer than its size explains is
    /// abandoned; either leaves part of the text plain instead of holding up the caller.
    pub fn highlight(
        &mut self,
        language: &Language,
        text: &str,
        cancel: Option<&AtomicBool>,
    ) -> Result<Vec<HighlightSpan>, SyntaxError> {
        let Some(spec) = language.grammar else {
            return Ok(Vec::new());
        };
        if text.len() > u32::MAX as usize {
            return Err(SyntaxError::TooLarge { bytes: text.len() });
        }
        let grammar = grammar::load(spec)?;
        self.collected.clear();
        let collected = self.collect_layers(grammar, text, cancel);
        let spans = collected.map(|()| flatten(&mut self.collected, text));
        // Keep the buffer for the next file, unless this one was unusually large.
        if self.collected.capacity() > KEEP_COLLECTED {
            self.collected = Vec::new();
        }
        spans
    }

    /// Parse and query every layer, leaving their spans in `self.collected`.
    fn collect_layers(
        &mut self,
        root: &'static Grammar,
        text: &str,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), SyntaxError> {
        let mut pending = vec![Layer {
            grammar: root,
            ranges: Vec::new(),
            depth: 0,
            parent: None,
        }];
        let mut layer_count = 1;
        while let Some(layer) = pending.pop() {
            check_cancelled(cancel)?;
            let started = Instant::now();
            let parse_time = parse_time_allowed(layer_len(&layer, text));
            let tree = parse_layer(
                &mut self.parser,
                &layer,
                text,
                &Watchdog::new(cancel, parse_time),
            )?;
            check_cancelled(cancel)?;
            let Some(tree) = tree else {
                continue;
            };
            let query_time = MIN_QUERY_TIME.max(started.elapsed() * QUERY_TIME_FACTOR);
            if layer.depth < MAX_INJECTION_DEPTH {
                let before = pending.len();
                let watchdog = Watchdog::new(cancel, query_time);
                collect_injections(
                    &mut self.cursor,
                    &layer,
                    &tree,
                    text,
                    &watchdog,
                    &mut pending,
                );
                check_cancelled(cancel)?;
                pending.truncate(before + MAX_LAYERS.saturating_sub(layer_count));
                layer_count += pending.len() - before;
            }
            let watchdog = Watchdog::new(cancel, query_time);
            let (cursor, run, collected) = (&mut self.cursor, &mut self.run, &mut self.collected);
            collect_highlights(cursor, run, &layer, &tree, text, &watchdog, collected);
            check_cancelled(cancel)?;
        }
        Ok(())
    }
}

fn check_cancelled(cancel: Option<&AtomicBool>) -> Result<(), SyntaxError> {
    match cancel {
        Some(flag) if flag.load(Ordering::Relaxed) => Err(SyntaxError::Cancelled),
        _ => Ok(()),
    }
}

/// Tells a running parse or query when to stop: the caller cancelled, or its time is up.
pub(crate) struct Watchdog<'a> {
    cancel: Option<&'a AtomicBool>,
    deadline: Instant,
}

impl<'a> Watchdog<'a> {
    pub(crate) fn new(cancel: Option<&'a AtomicBool>, allowed: Duration) -> Watchdog<'a> {
        Watchdog {
            cancel,
            deadline: Instant::now() + allowed,
        }
    }

    /// The answer for tree-sitter's progress callbacks, which it calls every hundred steps.
    pub(crate) fn check(&self) -> ControlFlow<()> {
        let cancelled = self.cancel.is_some_and(|flag| flag.load(Ordering::Relaxed));
        if cancelled || Instant::now() >= self.deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
}

/// Parse one layer. `Ok(None)` means there is no tree: the layer's ranges were unusable, or
/// the watchdog stopped the parse.
fn parse_layer(
    parser: &mut Parser,
    layer: &Layer,
    text: &str,
    watchdog: &Watchdog,
) -> Result<Option<Tree>, SyntaxError> {
    if parser.set_included_ranges(&layer.ranges).is_err() {
        return Ok(None);
    }
    parser
        .set_language(&layer.grammar.language)
        .map_err(|error| SyntaxError::Grammar {
            language: layer.grammar.spec.name,
            message: error.to_string(),
        })?;
    let bytes = text.as_bytes();
    let mut on_progress = |_: &ParseState| watchdog.check();
    let options = ParseOptions::new().progress_callback(&mut on_progress);
    let tree = parser.parse_with_options(
        &mut |offset, _| &bytes[offset.min(bytes.len())..],
        None,
        Some(options),
    );
    if tree.is_none() {
        // A parse that was cut short would otherwise resume on the next call.
        parser.reset();
    }
    Ok(tree)
}

/// How many bytes of the text a layer covers.
fn layer_len(layer: &Layer, text: &str) -> usize {
    if layer.ranges.is_empty() {
        return text.len();
    }
    layer
        .ranges
        .iter()
        .map(|range| range.end_byte.saturating_sub(range.start_byte))
        .sum()
}

/// How long the parse of `bytes` of text may take before it is given up on.
pub(crate) fn parse_time_allowed(bytes: usize) -> Duration {
    MIN_PARSE_TIME + PARSE_TIME_PER_BYTE.saturating_mul(u32::try_from(bytes).unwrap_or(u32::MAX))
}

/// The nodes to run a query from so that it covers the whole tree under `root` although no
/// match starts more than [`BAND_DEPTH`] levels below the node it was run from: `root`
/// itself, then every node one level past that limit, and so on down.
///
/// Finding them does not walk the tree: a subtree too small to reach the limit is skipped
/// by its node count, so for ordinary code this looks at a handful of nodes and returns
/// `root` alone.
pub(crate) fn band_roots(root: Node) -> Vec<Node> {
    let limit = BAND_DEPTH as usize;
    let mut roots = vec![root];
    let mut next = 0;
    while next < roots.len() {
        let band_root = roots[next];
        next += 1;
        // The deepest a subtree of n nodes can go is n - 1 levels.
        let reaches_past = |node: Node, depth: usize| depth + node.descendant_count() > limit + 1;
        if !reaches_past(band_root, 0) {
            continue;
        }
        let mut walker = band_root.walk();
        let mut depth = 0;
        'walk: loop {
            let node = walker.node();
            if depth == limit + 1 {
                roots.push(node);
            } else if reaches_past(node, depth) && walker.goto_first_child() {
                depth += 1;
                continue;
            }
            while !walker.goto_next_sibling() {
                if depth == 0 || !walker.goto_parent() {
                    break 'walk;
                }
                depth -= 1;
            }
            if depth == 0 {
                break;
            }
        }
    }
    roots
}

/// Whether the predicates tree-sitter leaves to us hold for a match.
fn predicates_hold(predicates: &[Predicate], matched: &QueryMatch, text: &str) -> bool {
    predicates.iter().all(|predicate| match predicate {
        Predicate::LuaMatch {
            capture,
            regex,
            negated,
        } => matched.nodes_for_capture_index(*capture).all(|node| {
            let is_match = text
                .get(node.byte_range())
                .is_some_and(|node_text| regex.is_match(node_text));
            is_match != *negated
        }),
    })
}

// ---------------------------------------------------------------------------------------
// Injections
// ---------------------------------------------------------------------------------------

/// One match of an injection pattern.
struct Injection {
    /// The range of the first content node: two patterns that inject the same node compete.
    key: (usize, usize),
    pattern: usize,
    spec: &'static GrammarSpec,
    pieces: Vec<Range>,
    combined: bool,
}

/// Run the layer's injections query and push a new layer for each injected document.
fn collect_injections(
    cursor: &mut QueryCursor,
    layer: &Layer,
    tree: &Tree,
    text: &str,
    watchdog: &Watchdog,
    pending: &mut Vec<Layer>,
) {
    let Some(injections) = &layer.grammar.injections else {
        return;
    };
    let own = layer.grammar.spec;
    let mut found: Vec<Injection> = Vec::new();
    for band_root in band_roots(tree.root_node()) {
        let mut on_progress = |_: &QueryCursorState| watchdog.check();
        let options = QueryCursorOptions::new().progress_callback(&mut on_progress);
        let query = &injections.query;
        let mut matches = cursor.matches_with_options(query, band_root, text.as_bytes(), options);
        while let Some(matched) = matches.next() {
            let pattern = &injections.patterns[matched.pattern_index];
            if !predicates_hold(&pattern.predicates, matched, text) {
                continue;
            }
            // A language named by the text (the word after a code fence) beats one set on
            // the pattern.
            let captured = injections
                .language_capture
                .and_then(|index| matched.nodes_for_capture_index(index).next());
            let spec = match (captured, pattern.language) {
                (Some(node), _) => text
                    .get(node.byte_range())
                    .and_then(registry::grammar_by_name),
                (None, Some(FixedLanguage::Named(spec))) => spec,
                (None, Some(FixedLanguage::Own)) => Some(own),
                (None, Some(FixedLanguage::Parent)) => layer.parent,
                (None, None) => None,
            };
            let Some(spec) = spec else { continue };

            let mut pieces = Vec::new();
            let mut key = None;
            for node in matched.nodes_for_capture_index(injections.content_capture) {
                key.get_or_insert((node.start_byte(), node.end_byte()));
                content_pieces(node, pattern.include_children, own.children, &mut pieces);
            }
            if let Some(key) = key {
                let (pattern, combined) = (matched.pattern_index, pattern.combined);
                found.push(Injection {
                    key,
                    pattern,
                    spec,
                    pieces,
                    combined,
                });
            }
        }
    }

    // When two patterns inject the same node, the grammar's precedence picks one, as it
    // does for highlights. The sort is stable, so equal keys stay in pattern order.
    found.sort_by_key(|injection| (injection.key, injection.pattern));
    let last_wins = own.precedence == Precedence::LastWins;
    let mut winners: Vec<Injection> = Vec::with_capacity(found.len());
    for injection in found {
        match winners.last_mut() {
            Some(previous) if previous.key == injection.key => {
                if last_wins {
                    *previous = injection;
                }
            }
            _ => winners.push(injection),
        }
    }

    // A combined pattern makes one document, per language, out of everything it matched.
    let mut documents: Vec<Injection> = Vec::with_capacity(winners.len());
    let mut combined_at: HashMap<(usize, *const GrammarSpec), usize> = HashMap::new();
    for mut injection in winners {
        if !injection.combined {
            documents.push(injection);
            continue;
        }
        let group = (injection.pattern, injection.spec as *const GrammarSpec);
        match combined_at.get(&group) {
            Some(&index) => documents[index].pieces.append(&mut injection.pieces),
            None => {
                combined_at.insert(group, documents.len());
                documents.push(injection);
            }
        }
    }

    for document in documents {
        let ranges = clip_to_parent(normalize_pieces(document.pieces), &layer.ranges);
        if ranges.is_empty() {
            continue;
        }
        if let Ok(grammar) = grammar::load(document.spec) {
            pending.push(Layer {
                grammar,
                ranges,
                depth: layer.depth + 1,
                parent: Some(own),
            });
        }
    }
}

/// The byte ranges of a content node that belong to the injected document: all of it, or
/// what is left after its children are cut out.
fn content_pieces(
    node: Node,
    include_children: bool,
    children: InjectedChildren,
    pieces: &mut Vec<Range>,
) {
    let mut start_byte = node.start_byte();
    let mut start_point = node.start_position();
    if !include_children && node.child_count() > 0 {
        let mut walker = node.walk();
        for child in node.children(&mut walker) {
            if children == InjectedChildren::ExcludeNamed && !child.is_named() {
                continue;
            }
            if child.start_byte() > start_byte {
                pieces.push(Range {
                    start_byte,
                    end_byte: child.start_byte(),
                    start_point,
                    end_point: child.start_position(),
                });
            }
            if child.end_byte() > start_byte {
                start_byte = child.end_byte();
                start_point = child.end_position();
            }
        }
    }
    if node.end_byte() > start_byte {
        pieces.push(Range {
            start_byte,
            end_byte: node.end_byte(),
            start_point,
            end_point: node.end_position(),
        });
    }
}

/// Put pieces in order and make them disjoint, as the parser requires.
fn normalize_pieces(mut pieces: Vec<Range>) -> Vec<Range> {
    pieces.sort_by_key(|piece| (piece.start_byte, piece.end_byte));
    let mut out: Vec<Range> = Vec::with_capacity(pieces.len());
    for mut piece in pieces {
        if let Some(last) = out.last() {
            if piece.end_byte <= last.end_byte {
                continue;
            }
            if piece.start_byte < last.end_byte {
                piece.start_byte = last.end_byte;
                piece.start_point = last.end_point;
            }
        }
        out.push(piece);
    }
    out
}

/// Keep only the parts of `pieces` that lie inside the parent layer's ranges. Both lists are
/// in order and disjoint; an empty parent list means the whole text.
fn clip_to_parent(pieces: Vec<Range>, parent: &[Range]) -> Vec<Range> {
    if parent.is_empty() {
        return pieces;
    }
    let mut out = Vec::with_capacity(pieces.len());
    let mut parent_index = 0;
    for piece in pieces {
        while parent_index < parent.len() && parent[parent_index].end_byte <= piece.start_byte {
            parent_index += 1;
        }
        for outer in &parent[parent_index..] {
            if outer.start_byte >= piece.end_byte {
                break;
            }
            let (start_byte, start_point) = if outer.start_byte > piece.start_byte {
                (outer.start_byte, outer.start_point)
            } else {
                (piece.start_byte, piece.start_point)
            };
            let (end_byte, end_point) = if outer.end_byte < piece.end_byte {
                (outer.end_byte, outer.end_point)
            } else {
                (piece.end_byte, piece.end_point)
            };
            if start_byte < end_byte {
                out.push(Range {
                    start_byte,
                    end_byte,
                    start_point,
                    end_point,
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Highlights and locals
// ---------------------------------------------------------------------------------------

/// The local variables in scope while a layer's captures go by.
///
/// A locals query marks scopes, the names defined in them and the identifiers that may refer
/// to a name. A reference to a local takes the highlight its definition got, and patterns
/// marked `(#is-not? local)` do not apply to it: that is how Ruby tells a variable from a
/// method call.
struct Locals<'t> {
    scopes: Vec<Scope>,
    /// The names defined in the open scopes, in order; each scope owns a suffix.
    names: Vec<&'t str>,
    /// For each name, its definitions in the open scopes, innermost last.
    definitions: HashMap<&'t str, Vec<Definition>>,
}

struct Scope {
    end: usize,
    /// The outermost enclosing scope whose names are visible from here.
    visible_from: usize,
    /// Where this scope's names start in `Locals::names`.
    names_start: usize,
}

struct Definition {
    scope: usize,
    value_end: usize,
    highlight: Option<CaptureId>,
}

impl<'t> Locals<'t> {
    fn new() -> Locals<'t> {
        let file = Scope {
            end: usize::MAX,
            visible_from: 0,
            names_start: 0,
        };
        Locals {
            scopes: vec![file],
            names: Vec::new(),
            definitions: HashMap::new(),
        }
    }

    /// Close the scopes that ended before `position`.
    fn leave_scopes_before(&mut self, position: usize) {
        while self.scopes.len() > 1 && self.scopes.last().is_some_and(|scope| position > scope.end)
        {
            let Some(scope) = self.scopes.pop() else {
                break;
            };
            for name in self.names.drain(scope.names_start..) {
                if let Some(definitions) = self.definitions.get_mut(name) {
                    definitions.pop();
                }
            }
        }
    }

    fn enter_scope(&mut self, end: usize, inherits: bool) {
        let index = self.scopes.len();
        let visible_from = match self.scopes.last() {
            Some(outer) if inherits => outer.visible_from,
            _ => index,
        };
        self.scopes.push(Scope {
            end,
            visible_from,
            names_start: self.names.len(),
        });
    }

    fn define(&mut self, name: &'t str, value_end: usize) {
        let scope = self.scopes.len() - 1;
        self.names.push(name);
        self.definitions.entry(name).or_default().push(Definition {
            scope,
            value_end,
            highlight: None,
        });
    }

    /// Record how the name defined last was highlighted.
    fn set_highlight(&mut self, name: &str, highlight: Option<CaptureId>) {
        if let Some(definition) = self
            .definitions
            .get_mut(name)
            .and_then(|definitions| definitions.last_mut())
        {
            definition.highlight = highlight;
        }
    }

    /// The highlight of the definition a reference at `position` refers to. The outer
    /// `None` means the name is not a local here.
    fn lookup(&self, name: &str, position: usize) -> Option<Option<CaptureId>> {
        let visible_from = self.scopes.last().map_or(0, |scope| scope.visible_from);
        self.definitions
            .get(name)?
            .iter()
            .rev()
            .take_while(|definition| definition.scope >= visible_from)
            .find(|definition| position >= definition.value_end)
            .map(|definition| definition.highlight)
    }
}

/// Run the layer's highlights (and locals) query and add its spans to `collected`.
fn collect_highlights(
    cursor: &mut QueryCursor,
    run: &mut Vec<RunCapture>,
    layer: &Layer,
    tree: &Tree,
    text: &str,
    watchdog: &Watchdog,
    collected: &mut Vec<Collected>,
) {
    run.clear();
    let mut builder = SpanBuilder {
        layer,
        text,
        locals: (layer.grammar.locals_patterns > 0).then(Locals::new),
        run,
        collected,
    };
    let roots = band_roots(tree.root_node());
    if let [root] = roots[..] {
        // The usual case: the captures arrive in order and are decided as they come.
        query_captures(cursor, layer.grammar, root, text, watchdog, |capture| {
            builder.push(capture)
        });
    } else {
        // A tree deep enough to need several runs of the query: each run is in order, but
        // the runs interleave, so gather them all and put them in order first.
        let mut gathered: Vec<RunCapture> = Vec::new();
        for root in roots {
            query_captures(cursor, layer.grammar, root, text, watchdog, |capture| {
                gathered.push(capture)
            });
        }
        gathered.sort_by_key(|capture| capture.start);
        for capture in gathered {
            builder.push(capture);
        }
    }
    builder.finish_run();
}

/// Run the highlights (and locals) query from `root` and hand each capture to `sink`.
fn query_captures(
    cursor: &mut QueryCursor,
    grammar: &Grammar,
    root: Node,
    text: &str,
    watchdog: &Watchdog,
    mut sink: impl FnMut(RunCapture),
) {
    let mut on_progress = |_: &QueryCursorState| watchdog.check();
    let options = QueryCursorOptions::new().progress_callback(&mut on_progress);
    let mut captures = cursor.captures_with_options(&grammar.query, root, text.as_bytes(), options);
    while let Some((matched, index)) = captures.next() {
        let pattern = matched.pattern_index;
        let predicates = &grammar.patterns[pattern].predicates;
        if !predicates.is_empty() && !predicates_hold(predicates, matched, text) {
            continue;
        }
        let capture = matched.captures()[*index];
        let mut value_end = 0;
        let is_definition = pattern < grammar.locals_patterns
            && grammar.local_roles[capture.index as usize] == LocalRole::Definition;
        if is_definition {
            let value = matched.captures().iter().find(|other| {
                grammar.local_roles[other.index as usize] == LocalRole::DefinitionValue
            });
            value_end = value.map_or(0, |value| value.node.end_byte());
        }
        sink(RunCapture {
            start: capture.node.start_byte(),
            end: capture.node.end_byte(),
            pattern,
            capture: capture.index,
            value_end,
        });
    }
}

/// Turns a layer's captures, fed in order of where they start, into spans.
///
/// Everything that starts at one byte is decided together, because which capture wins a
/// node depends on the others that captured it.
struct SpanBuilder<'a, 't> {
    layer: &'a Layer,
    text: &'t str,
    locals: Option<Locals<'t>>,
    /// The captures that start at the byte being decided.
    run: &'a mut Vec<RunCapture>,
    collected: &'a mut Vec<Collected>,
}

impl SpanBuilder<'_, '_> {
    fn push(&mut self, capture: RunCapture) {
        if self
            .run
            .first()
            .is_some_and(|first| first.start != capture.start)
        {
            self.finish_run();
        }
        self.run.push(capture);
    }

    /// Decide the captures waiting in `run`: one highlight per distinct range.
    fn finish_run(&mut self) {
        let Some(start) = self.run.first().map(|capture| capture.start) else {
            return;
        };
        let (grammar, text) = (self.layer.grammar, self.text);
        // Wider nodes first, so a scope is entered before the names inside it are looked
        // at; within one node, pattern order. The sort is stable, so two captures of one
        // pattern stay in the order the pattern wrote them.
        self.run
            .sort_by(|a, b| b.end.cmp(&a.end).then(a.pattern.cmp(&b.pattern)));
        if let Some(locals) = &mut self.locals {
            locals.leave_scopes_before(start);
        }
        let mut group_start = 0;
        while group_start < self.run.len() {
            let end = self.run[group_start].end;
            let group_len = self.run[group_start..]
                .iter()
                .take_while(|capture| capture.end == end)
                .count();
            let group = &self.run[group_start..group_start + group_len];
            group_start += group_len;

            let name = text.get(start..end).unwrap_or("");
            let mut defined = false;
            let mut referenced: Option<Option<CaptureId>> = None;
            if let Some(locals) = &mut self.locals {
                for capture in group
                    .iter()
                    .filter(|capture| capture.pattern < grammar.locals_patterns)
                {
                    match grammar.local_roles[capture.capture as usize] {
                        LocalRole::Scope => {
                            let inherits = grammar.patterns[capture.pattern].scope_inherits;
                            locals.enter_scope(end, inherits);
                        }
                        LocalRole::Definition => {
                            locals.define(name, capture.value_end);
                            defined = true;
                            referenced = None;
                        }
                        LocalRole::Reference if !defined => referenced = locals.lookup(name, start),
                        _ => {}
                    }
                }
            }
            let inherited = referenced.flatten();
            let is_local = defined || inherited.is_some();

            let mut candidates = group.iter().filter(|capture| {
                capture.pattern >= grammar.locals_patterns
                    && grammar.capture_ids[capture.capture as usize].is_some()
                    && !(is_local && grammar.patterns[capture.pattern].non_local)
            });
            let winner = match grammar.spec.precedence {
                Precedence::LastWins => candidates.next_back(),
                Precedence::FirstWins => candidates.next(),
            };
            let own = winner.and_then(|capture| grammar.capture_ids[capture.capture as usize]);
            if defined {
                if let Some(locals) = &mut self.locals {
                    locals.set_highlight(name, own);
                }
            }
            if let Some(capture) = inherited.or(own) {
                if start < end {
                    self.collected.push(Collected {
                        start: start as u32,
                        end: end as u32,
                        depth: self.layer.depth,
                        capture,
                    });
                }
            }
        }
        self.run.clear();
    }
}

// ---------------------------------------------------------------------------------------
// Flattening
// ---------------------------------------------------------------------------------------

/// A span in the sweep, ordered so that the greatest is the innermost: it starts last, then
/// ends first, then comes from the deepest layer, then was collected last.
type Active = (u32, Reverse<u32>, u16, u32, CaptureId);

/// Turn every layer's spans into one sorted list where each byte has the innermost capture.
///
/// Spans from one layer nest, but spans from different layers can overlap in any way, so
/// this is a sweep over the text with the active spans in a heap rather than a stack.
fn flatten(collected: &mut [Collected], text: &str) -> Vec<HighlightSpan> {
    // Stable, so spans that start together keep the order they were collected in.
    collected.sort_by_key(|span| span.start);
    let mut out: Vec<HighlightSpan> = Vec::with_capacity(collected.len());
    let mut active: BinaryHeap<Active> = BinaryHeap::new();
    let mut next = 0;
    let mut position = 0u32;
    loop {
        while active.peek().is_some_and(|top| top.1 .0 <= position) {
            active.pop();
        }
        let upcoming = collected.get(next).map(|span| span.start);
        let Some(top) = active.peek() else {
            // Nothing is open: jump to the next span, or finish.
            let Some(start) = upcoming else { break };
            position = start;
            next = activate(collected, next, position, &mut active);
            continue;
        };
        let segment_end = upcoming.map_or(top.1 .0, |start| start.min(top.1 .0));
        push_span(&mut out, position, segment_end, top.4, text);
        position = segment_end;
        next = activate(collected, next, position, &mut active);
    }
    out
}

/// Move the spans that start at `position` into the heap; returns the index of the first
/// one that starts later.
fn activate(
    collected: &[Collected],
    mut next: usize,
    position: u32,
    active: &mut BinaryHeap<Active>,
) -> usize {
    while let Some(span) = collected.get(next).filter(|span| span.start <= position) {
        active.push((
            span.start,
            Reverse(span.end),
            span.depth,
            next as u32,
            span.capture,
        ));
        next += 1;
    }
    next
}

/// Append a span, joining it to the previous one when they touch and agree. The ends are
/// clamped into the text and moved back onto character boundaries, which tree-sitter's
/// nodes are on already unless a grammar's scanner misbehaves.
fn push_span(out: &mut Vec<HighlightSpan>, start: u32, end: u32, capture: CaptureId, text: &str) {
    let start = floor_char_boundary(text, start as usize);
    let end = floor_char_boundary(text, end as usize);
    if start >= end {
        return;
    }
    match out.last_mut() {
        Some(last) if last.end == start && last.capture == capture => last.end = end,
        Some(last) if last.end > start => {}
        _ => out.push(HighlightSpan {
            start,
            end,
            capture,
        }),
    }
}

/// The nearest character boundary at or before `index`, or the end of the text.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::intern;

    fn collected(start: u32, end: u32, depth: u16, name: &str) -> Collected {
        Collected {
            start,
            end,
            depth,
            capture: intern(name).unwrap(),
        }
    }

    fn flat(spans: &[Collected], text: &str) -> Vec<(usize, usize, &'static str)> {
        let mut spans = spans.to_vec();
        flatten(&mut spans, text)
            .into_iter()
            .map(|span| (span.start, span.end, span.capture.name()))
            .collect()
    }

    const TEXT: &str = "0123456789abcdefghij";

    #[test]
    fn nested_spans_split_the_outer_one() {
        let spans = [
            collected(0, 10, 0, "string"),
            collected(3, 5, 0, "string.escape"),
        ];
        assert_eq!(
            flat(&spans, TEXT),
            [(0, 3, "string"), (3, 5, "string.escape"), (5, 10, "string")]
        );
    }

    #[test]
    fn the_innermost_of_several_wins() {
        let spans = [
            collected(0, 20, 0, "comment"),
            collected(2, 12, 0, "string"),
            collected(4, 6, 0, "keyword"),
            collected(6, 8, 0, "number"),
        ];
        assert_eq!(
            flat(&spans, TEXT),
            [
                (0, 2, "comment"),
                (2, 4, "string"),
                (4, 6, "keyword"),
                (6, 8, "number"),
                (8, 12, "string"),
                (12, 20, "comment"),
            ]
        );
    }

    #[test]
    fn spans_that_start_together_nest_by_length_whatever_their_order() {
        let outer_first = [collected(2, 9, 0, "title"), collected(2, 5, 1, "string")];
        let inner_first = [collected(2, 5, 1, "string"), collected(2, 9, 0, "title")];
        let expected = [(2, 5, "string"), (5, 9, "title")];
        assert_eq!(flat(&outer_first, TEXT), expected);
        assert_eq!(flat(&inner_first, TEXT), expected);
    }

    #[test]
    fn the_deeper_layer_wins_an_identical_range() {
        let spans = [
            collected(1, 4, 1, "function"),
            collected(1, 4, 0, "constructor"),
        ];
        assert_eq!(flat(&spans, TEXT), [(1, 4, "function")]);
        let later = [
            collected(1, 4, 0, "variable"),
            collected(1, 4, 0, "property"),
        ];
        assert_eq!(flat(&later, TEXT), [(1, 4, "property")]);
    }

    #[test]
    fn overlapping_spans_from_different_layers_do_not_lose_text() {
        // Not nested: the later start wins while it lasts, then the earlier one resumes.
        let spans = [collected(0, 6, 0, "string"), collected(4, 10, 1, "keyword")];
        assert_eq!(flat(&spans, TEXT), [(0, 4, "string"), (4, 10, "keyword")]);
        let spans = [
            collected(0, 10, 0, "string"),
            collected(2, 6, 1, "keyword"),
            collected(4, 8, 2, "number"),
        ];
        assert_eq!(
            flat(&spans, TEXT),
            [
                (0, 2, "string"),
                (2, 4, "keyword"),
                (4, 8, "number"),
                (8, 10, "string")
            ]
        );
    }

    #[test]
    fn touching_spans_with_one_capture_are_joined() {
        let spans = [
            collected(0, 1, 0, "punctuation.bracket"),
            collected(1, 2, 0, "punctuation.bracket"),
        ];
        assert_eq!(flat(&spans, TEXT), [(0, 2, "punctuation.bracket")]);
        let apart = [
            collected(0, 1, 0, "punctuation.bracket"),
            collected(2, 3, 0, "punctuation.bracket"),
        ];
        assert_eq!(flat(&apart, TEXT).len(), 2);
    }

    #[test]
    fn gaps_stay_unhighlighted_and_input_order_does_not_matter() {
        let spans = [
            collected(12, 14, 0, "number"),
            collected(1, 3, 0, "keyword"),
            collected(6, 7, 0, "string"),
        ];
        assert_eq!(
            flat(&spans, TEXT),
            [(1, 3, "keyword"), (6, 7, "string"), (12, 14, "number")]
        );
        assert_eq!(flat(&[], TEXT), []);
    }

    #[test]
    fn spans_are_kept_inside_the_text_and_on_character_boundaries() {
        // "é" is two bytes: an end or a start inside it is pulled back to its first byte.
        let text = "aé";
        assert_eq!(
            flat(&[collected(0, 2, 0, "string")], text),
            [(0, 1, "string")]
        );
        assert_eq!(
            flat(&[collected(2, 3, 0, "string")], text),
            [(1, 3, "string")]
        );
        assert_eq!(
            flat(
                &[collected(0, 2, 0, "string"), collected(2, 3, 0, "number")],
                text
            ),
            [(0, 1, "string"), (1, 3, "number")]
        );
        assert_eq!(
            flat(&[collected(0, 99, 0, "string")], text),
            [(0, 3, "string")]
        );
        assert_eq!(flat(&[collected(50, 60, 0, "string")], text), []);
    }

    #[test]
    fn finds_the_spans_in_a_byte_range() {
        let keyword = intern("keyword").unwrap();
        let spans: Vec<HighlightSpan> = [(0, 3), (5, 8), (8, 10), (15, 20)]
            .map(|(start, end)| HighlightSpan {
                start,
                end,
                capture: keyword,
            })
            .to_vec();
        let found = |range: ByteRange<usize>| -> Vec<(usize, usize)> {
            spans_in(&spans, range)
                .iter()
                .map(|span| (span.start, span.end))
                .collect()
        };
        assert_eq!(found(0..20), [(0, 3), (5, 8), (8, 10), (15, 20)]);
        assert_eq!(found(6..9), [(5, 8), (8, 10)]);
        assert_eq!(found(3..5), []);
        assert_eq!(found(8..8), []);
        assert_eq!(found(10..15), []);
        assert_eq!(found(19..100), [(15, 20)]);
        assert_eq!(found(100..200), []);
        assert!(spans_in(&[], 0..10).is_empty());
        assert_eq!(spans[1].range(), 5..8);
    }

    #[test]
    fn locals_resolve_through_scopes() {
        let function = intern("function").unwrap();
        let variable = intern("variable").unwrap();
        let mut locals = Locals::new();
        assert_eq!(locals.lookup("x", 0), None);
        locals.define("x", 0);
        locals.set_highlight("x", Some(variable));
        assert_eq!(locals.lookup("x", 5), Some(Some(variable)));

        // An inheriting scope sees the outer name and can shadow it.
        locals.enter_scope(50, true);
        assert_eq!(locals.lookup("x", 12), Some(Some(variable)));
        locals.define("x", 20);
        locals.set_highlight("x", Some(function));
        // Inside the definition's own value the outer one still applies.
        assert_eq!(locals.lookup("x", 15), Some(Some(variable)));
        assert_eq!(locals.lookup("x", 25), Some(Some(function)));

        // A scope that does not inherit sees neither.
        locals.enter_scope(40, false);
        assert_eq!(locals.lookup("x", 30), None);
        locals.define("y", 0);
        assert_eq!(locals.lookup("y", 31), Some(None));

        // Leaving scopes forgets their names.
        locals.leave_scopes_before(41);
        assert_eq!(locals.lookup("y", 41), None);
        assert_eq!(locals.lookup("x", 41), Some(Some(function)));
        locals.leave_scopes_before(51);
        assert_eq!(locals.lookup("x", 51), Some(Some(variable)));
        // The file scope is never left.
        locals.leave_scopes_before(usize::MAX);
        assert_eq!(locals.lookup("x", 60), Some(Some(variable)));
    }

    fn range(start: usize, end: usize) -> Range {
        Range {
            start_byte: start,
            end_byte: end,
            start_point: tree_sitter::Point::new(0, start),
            end_point: tree_sitter::Point::new(0, end),
        }
    }

    fn bytes(ranges: &[Range]) -> Vec<(usize, usize)> {
        ranges
            .iter()
            .map(|range| (range.start_byte, range.end_byte))
            .collect()
    }

    #[test]
    fn pieces_are_ordered_and_made_disjoint() {
        let pieces = vec![
            range(10, 20),
            range(0, 5),
            range(3, 8),
            range(12, 15),
            range(18, 25),
        ];
        let normal = normalize_pieces(pieces);
        assert_eq!(bytes(&normal), [(0, 5), (5, 8), (10, 20), (20, 25)]);
        assert_eq!(normal[1].start_point.column, 5);
        assert!(normalize_pieces(Vec::new()).is_empty());
    }

    #[test]
    fn pieces_are_clipped_to_the_parent_layer() {
        let parent = [range(5, 10), range(20, 30)];
        let clipped = clip_to_parent(
            vec![range(0, 7), range(8, 22), range(28, 40), range(50, 60)],
            &parent,
        );
        assert_eq!(bytes(&clipped), [(5, 7), (8, 10), (20, 22), (28, 30)]);
        assert_eq!(clipped[0].start_point.column, 5);
        assert_eq!(clipped[3].end_point.column, 30);
        // No parent ranges means the parent covers everything.
        assert_eq!(bytes(&clip_to_parent(vec![range(1, 2)], &[])), [(1, 2)]);
    }
    fn parse_javascript(text: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .unwrap();
        parser.parse(text, None).unwrap()
    }

    #[test]
    fn a_shallow_tree_is_one_band() {
        for text in ["", "const x = [1, [2, [3]]];\nfunction f() { return x; }\n"] {
            let tree = parse_javascript(text);
            assert_eq!(band_roots(tree.root_node()), [tree.root_node()]);
        }
        // Wide is not deep: ten thousand siblings are still one band.
        let wide = parse_javascript(&"f(1);\n".repeat(10_000));
        assert_eq!(band_roots(wide.root_node()).len(), 1);
    }

    #[test]
    fn a_deep_tree_is_cut_exactly_where_a_query_stops_starting_matches() {
        // A thousand nested arrays with a number at the bottom: one node starts at each byte
        // up to the number.
        let depth = 1000;
        let text = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        let tree = parse_javascript(&text);
        let roots = band_roots(tree.root_node());
        assert!(
            roots.len() >= depth / (BAND_DEPTH as usize + 1),
            "{} bands",
            roots.len()
        );

        // Every later root is one level past the limit below another root.
        for band_root in &roots[1..] {
            let mut levels = 1;
            let mut above = band_root.parent();
            while let Some(node) = above.filter(|node| !roots.contains(node)) {
                above = node.parent();
                levels += 1;
            }
            assert!(above.is_some(), "a band root with no band root above it");
            assert_eq!(levels, BAND_DEPTH as usize + 1);
        }

        // A query limited to that depth, run from every root, sees each node exactly once.
        let language: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        let query = tree_sitter::Query::new(&language, "(array) @array (number) @number").unwrap();
        let mut cursor = QueryCursor::new();
        cursor.set_max_start_depth(Some(BAND_DEPTH));
        let mut starts = Vec::new();
        for band_root in &roots {
            let mut captures = cursor.captures(&query, *band_root, text.as_bytes());
            while let Some((matched, index)) = captures.next() {
                starts.push(matched.captures()[*index].node.start_byte());
            }
        }
        starts.sort_unstable();
        assert_eq!(starts, (0..=depth).collect::<Vec<_>>());
    }

    #[test]
    fn a_watchdog_stops_on_cancel_and_when_its_time_is_up() {
        let flag = AtomicBool::new(false);
        let patient = Watchdog::new(Some(&flag), Duration::from_secs(3600));
        assert!(patient.check().is_continue());
        flag.store(true, Ordering::Relaxed);
        assert!(patient.check().is_break());
        assert!(Watchdog::new(None, Duration::from_secs(3600))
            .check()
            .is_continue());
        assert!(Watchdog::new(None, Duration::ZERO).check().is_break());
    }

    #[test]
    fn the_time_a_parse_may_take_grows_with_the_text() {
        assert_eq!(parse_time_allowed(0), MIN_PARSE_TIME);
        assert_eq!(
            parse_time_allowed(1_000_000),
            MIN_PARSE_TIME + Duration::from_secs(2)
        );
        assert!(parse_time_allowed(usize::MAX) > Duration::from_secs(3600));
    }

    #[test]
    fn a_layer_that_runs_out_of_time_is_skipped_and_the_parser_stays_usable() {
        let grammar = grammar::load(&grammar::JAVASCRIPT).unwrap();
        let layer = Layer {
            grammar,
            ranges: Vec::new(),
            depth: 0,
            parent: None,
        };
        let text = "const answer = compute(1, [2, 3], { four: 5 });\n".repeat(4_000);
        let mut parser = Parser::new();
        let out_of_time = Watchdog::new(None, Duration::ZERO);
        assert!(parse_layer(&mut parser, &layer, &text, &out_of_time)
            .unwrap()
            .is_none());
        let patient = Watchdog::new(None, Duration::from_secs(3600));
        let tree = parse_layer(&mut parser, &layer, &text, &patient)
            .unwrap()
            .unwrap();
        assert!(!tree.root_node().has_error());
        assert_eq!(tree.root_node().end_byte(), text.len());

        // A query that is out of time before it starts leaves what it has, which is nothing,
        // and does not disturb the next one.
        let mut cursor = QueryCursor::new();
        let (mut run, mut collected, mut pending) = (Vec::new(), Vec::new(), Vec::new());
        collect_highlights(
            &mut cursor,
            &mut run,
            &layer,
            &tree,
            &text,
            &out_of_time,
            &mut collected,
        );
        collect_injections(
            &mut cursor,
            &layer,
            &tree,
            &text,
            &out_of_time,
            &mut pending,
        );
        assert!(
            collected.len() < 1_000,
            "{} spans from a query with no time",
            collected.len()
        );
        collected.clear();
        collect_highlights(
            &mut cursor,
            &mut run,
            &layer,
            &tree,
            &text,
            &patient,
            &mut collected,
        );
        assert!(collected.len() > 40_000, "{} spans", collected.len());
    }
}
