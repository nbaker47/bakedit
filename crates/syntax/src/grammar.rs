//! Grammars: a tree-sitter language and the queries that drive highlighting, loaded on
//! first use.
//!
//! Every grammar crate bundles a highlights query and some bundle injections and locals
//! queries. They are used as shipped, with three per-grammar decisions made here because the
//! queries were written for different highlighters:
//!
//! * [`Precedence`]: when two patterns capture the same node, which one wins.
//! * [`InjectedChildren`]: what an injection leaves out of the injected document.
//! * A few lines of extra patterns (the `*_EXTRA` constants) where a bundled query has an
//!   outright bug or files a literal under a name the theme does not colour the way Zed
//!   does.

use std::sync::OnceLock;

use regex::Regex;
use tree_sitter::{Query, QueryPredicateArg};

use crate::capture::{self, CaptureId};
use crate::error::SyntaxError;
use crate::lua_pattern;
use crate::outline::{self, OutlineRule};
use crate::registry;

/// Which pattern wins when several capture the same node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Precedence {
    /// The pattern written last. Queries in this style go from general to specific:
    /// `(identifier) @variable` first, the function-call patterns after it. This is what
    /// tree-sitter's own highlighter and Neovim do today.
    LastWins,
    /// The pattern written first. Queries in this style go from specific to general, the way
    /// tree-sitter's highlighter worked before 0.25 and Helix still does.
    FirstWins,
}

/// What an injection excludes from the injected document when the query does not say
/// `injection.include-children`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InjectedChildren {
    /// Every child of the content node, as tree-sitter's highlighter does.
    ExcludeAll,
    /// Only the named children, as Neovim does. tree-sitter-md's queries are written for
    /// this: an `inline` node's children are anonymous tokens except for the block
    /// continuation markers, and only those markers are meant to be cut out.
    ExcludeNamed,
}

/// Everything needed to build one grammar. The compiled form is cached in `loaded`.
pub(crate) struct GrammarSpec {
    /// The name other grammars inject this one by (`rust`, `markdown_inline`).
    pub name: &'static str,
    /// The tree-sitter language.
    pub language: fn() -> tree_sitter::Language,
    /// Highlight queries, concatenated in order.
    pub highlights: &'static [&'static str],
    /// Injection queries, concatenated in order.
    pub injections: &'static [&'static str],
    /// Local-variable queries, concatenated in order.
    pub locals: &'static [&'static str],
    /// How the highlight queries resolve two captures of one node.
    pub precedence: Precedence,
    /// How the injection queries treat the children of a content node.
    pub children: InjectedChildren,
    /// The symbols the outline lists, or empty for no outline.
    pub outline: &'static [OutlineRule],
    loaded: OnceLock<Result<Grammar, String>>,
    outline_loaded: OnceLock<Option<outline::OutlineQuery>>,
}

/// What a capture in the locals part of a query means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalRole {
    /// Not a locals capture.
    None,
    /// `local.scope`: the node opens a scope.
    Scope,
    /// `local.definition`: the node names a new local.
    Definition,
    /// `local.definition-value`: the node is the value a definition is initialised with; the
    /// definition is not visible inside it.
    DefinitionValue,
    /// `local.reference`: the node may refer to a local.
    Reference,
}

/// A predicate tree-sitter leaves to its caller.
#[derive(Debug)]
pub(crate) enum Predicate {
    /// `#lua-match?` and `#not-lua-match?`: every node of `capture` must (or must not) match.
    LuaMatch {
        capture: u32,
        regex: Regex,
        negated: bool,
    },
}

/// Per-pattern facts about the combined locals and highlights query.
#[derive(Debug, Default)]
pub(crate) struct PatternInfo {
    /// The pattern says `(#is-not? local)`: it does not apply to a local variable.
    pub non_local: bool,
    /// For a scope pattern: whether names from enclosing scopes are visible inside.
    pub scope_inherits: bool,
    /// Predicates to evaluate before the pattern counts as matched.
    pub predicates: Vec<Predicate>,
}

/// The language an injection pattern names with `#set!`.
#[derive(Clone, Copy)]
pub(crate) enum FixedLanguage {
    /// `injection.language`, already looked up (`None`: a language this crate does not have).
    Named(Option<&'static GrammarSpec>),
    /// `injection.self`: the language of the layer the pattern matched in.
    Own,
    /// `injection.parent`: the language of the layer that injected this one.
    Parent,
}

/// Per-pattern facts about the injections query.
pub(crate) struct InjectionPattern {
    /// The language set on the pattern, used when the match captures none.
    pub language: Option<FixedLanguage>,
    /// `injection.combined`: all the content nodes this pattern matches form one document.
    pub combined: bool,
    /// `injection.include-children`: the content node's children are part of the document.
    pub include_children: bool,
    /// Predicates to evaluate before the pattern counts as matched.
    pub predicates: Vec<Predicate>,
}

/// A compiled injections query.
pub(crate) struct Injections {
    pub query: Query,
    /// The index of `@injection.content`.
    pub content_capture: u32,
    /// The index of `@injection.language`, if any pattern captures one.
    pub language_capture: Option<u32>,
    pub patterns: Vec<InjectionPattern>,
}

/// A loaded grammar. Immutable and shared between threads.
pub(crate) struct Grammar {
    pub spec: &'static GrammarSpec,
    pub language: tree_sitter::Language,
    /// The locals patterns followed by the highlight patterns, as one query so their captures
    /// arrive interleaved in document order.
    pub query: Query,
    /// Patterns below this index are locals patterns.
    pub locals_patterns: usize,
    /// By capture index: the highlight the capture stands for, if it is one.
    pub capture_ids: Vec<Option<CaptureId>>,
    /// By capture index: what the capture means in a locals pattern.
    pub local_roles: Vec<LocalRole>,
    /// By pattern index.
    pub patterns: Vec<PatternInfo>,
    pub injections: Option<Injections>,
}

impl std::fmt::Debug for GrammarSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrammarSpec")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl GrammarSpec {
    /// Whether the outline query has been compiled (or found not to exist) already.
    #[cfg(test)]
    pub(crate) fn outline_is_loaded(&self) -> bool {
        self.outline_loaded.get().is_some()
    }

    /// The outline query, compiled on first use. `None` when the grammar has no outline or
    /// its rules fail to compile.
    pub(crate) fn outline_query(&'static self) -> Option<&'static outline::OutlineQuery> {
        self.outline_loaded
            .get_or_init(|| {
                let grammar = load(self).ok()?;
                outline::OutlineQuery::compile(&grammar.language, self.outline)
            })
            .as_ref()
    }
}

/// The compiled grammar for `spec`, building it on first use.
pub(crate) fn load(spec: &'static GrammarSpec) -> Result<&'static Grammar, SyntaxError> {
    spec.loaded
        .get_or_init(|| build(spec))
        .as_ref()
        .map_err(|message| SyntaxError::Grammar {
            language: spec.name,
            message: message.clone(),
        })
}

fn build(spec: &'static GrammarSpec) -> Result<Grammar, String> {
    let language = (spec.language)();
    let locals = spec.locals.join("\n");
    let highlights = spec.highlights.join("\n");
    let (source, locals_end) = if locals.is_empty() {
        (highlights, 0)
    } else {
        (format!("{locals}\n{highlights}"), locals.len() + 1)
    };
    let mut query = Query::new(&language, &source).map_err(|e| format!("highlights query: {e}"))?;
    let locals_patterns = (0..query.pattern_count())
        .filter(|&i| query.start_byte_for_pattern(i) < locals_end)
        .count();

    let capture_ids = query
        .capture_names()
        .iter()
        .map(|name| capture::normalize(name).and_then(|name| capture::intern(&name)))
        .collect();
    let local_roles = query
        .capture_names()
        .iter()
        .map(|name| local_role(name))
        .collect();

    let mut patterns = Vec::with_capacity(query.pattern_count());
    for index in 0..query.pattern_count() {
        let non_local = query
            .property_predicates(index)
            .iter()
            .any(|(property, positive)| !*positive && &*property.key == "local");
        let scope_inherits = query
            .property_settings(index)
            .iter()
            .find(|property| &*property.key == "local.scope-inherits")
            .is_none_or(|property| {
                property
                    .value
                    .as_deref()
                    .is_none_or(|value| value == "true")
            });
        let predicates = match custom_predicates(&query, index) {
            Some(predicates) => predicates,
            None => {
                query.disable_pattern(index);
                Vec::new()
            }
        };
        patterns.push(PatternInfo {
            non_local,
            scope_inherits,
            predicates,
        });
    }

    let injections = build_injections(spec, &language)?;
    Ok(Grammar {
        spec,
        language,
        query,
        locals_patterns,
        capture_ids,
        local_roles,
        patterns,
        injections,
    })
}

fn build_injections(
    spec: &'static GrammarSpec,
    language: &tree_sitter::Language,
) -> Result<Option<Injections>, String> {
    if spec.injections.is_empty() {
        return Ok(None);
    }
    let source = spec.injections.join("\n");
    let mut query = Query::new(language, &source).map_err(|e| format!("injections query: {e}"))?;
    let Some(content_capture) = query.capture_index_for_name("injection.content") else {
        return Ok(None);
    };
    let language_capture = query.capture_index_for_name("injection.language");
    let mut patterns = Vec::with_capacity(query.pattern_count());
    for index in 0..query.pattern_count() {
        let mut pattern = InjectionPattern {
            language: None,
            combined: false,
            include_children: false,
            predicates: Vec::new(),
        };
        for property in query.property_settings(index) {
            match &*property.key {
                "injection.language" => {
                    let named = property
                        .value
                        .as_deref()
                        .and_then(registry::grammar_by_name);
                    pattern.language = Some(FixedLanguage::Named(named));
                }
                "injection.self" => pattern.language = Some(FixedLanguage::Own),
                "injection.parent" => pattern.language = Some(FixedLanguage::Parent),
                "injection.combined" => pattern.combined = true,
                "injection.include-children" => pattern.include_children = true,
                _ => {}
            }
        }
        match custom_predicates(&query, index) {
            Some(predicates) => pattern.predicates = predicates,
            None => query.disable_pattern(index),
        }
        patterns.push(pattern);
    }
    Ok(Some(Injections {
        query,
        content_capture,
        language_capture,
        patterns,
    }))
}

/// What a capture name means inside a locals pattern. Neovim's queries qualify definitions
/// (`local.definition.function`); the qualifier does not matter here.
fn local_role(name: &str) -> LocalRole {
    match name {
        "local.scope" => LocalRole::Scope,
        "local.definition-value" => LocalRole::DefinitionValue,
        "local.reference" => LocalRole::Reference,
        _ if name == "local.definition" || name.starts_with("local.definition.") => {
            LocalRole::Definition
        }
        _ => LocalRole::None,
    }
}

/// The predicates of one pattern that tree-sitter does not evaluate itself.
///
/// Directives (`#offset!`, `#strip!`, ...) change nothing about whether a pattern matches
/// and are skipped. A test this crate does not implement returns `None`: the caller disables
/// the pattern, because applying it untested would colour every node it structurally
/// matches.
fn custom_predicates(query: &Query, pattern: usize) -> Option<Vec<Predicate>> {
    let mut predicates = Vec::new();
    for predicate in query.general_predicates(pattern) {
        match &*predicate.operator {
            operator @ ("lua-match?" | "not-lua-match?") => {
                let [QueryPredicateArg::Capture(capture), QueryPredicateArg::String(source)] =
                    &*predicate.args
                else {
                    return None;
                };
                let regex = lua_pattern::compile(source)?;
                predicates.push(Predicate::LuaMatch {
                    capture: *capture,
                    regex,
                    negated: operator.starts_with("not-"),
                });
            }
            operator if operator.ends_with('?') => return None,
            _ => {}
        }
    }
    Some(predicates)
}

// ---------------------------------------------------------------------------------------
// Extra patterns, added to a grammar's bundled highlights.
//
// Each one is here for one of two reasons: the bundled query has a bug, or it files
// something under a capture that Zed's own queries, and so the user's theme, draw
// differently. They go after the bundled query where the last pattern wins and before it
// where the first one does.
// ---------------------------------------------------------------------------------------

/// tree-sitter-rust:
///
/// * Numbers and booleans are filed under `constant.builtin`; Zed has `number` and `boolean`.
/// * The all-caps pattern never matches because of a stray quote in its regex
///   (`"^[A-Z][A-Z\\d_]+$'"`), so constants come out as constructors.
/// * Only `*`, `&` and `'` are captured as operators. The list here is the one Zed uses. It
///   includes `<` and `>`, so the patterns that make them brackets around generics are
///   repeated after it.
const RUST_EXTRA: &str = r#"
[(integer_literal) (float_literal)] @number
(boolean_literal) @boolean
((identifier) @constant
 (#match? @constant "^[A-Z][A-Z\\d_]+$"))

[
  "!=" "%" "%=" "&" "&=" "&&" "*" "*=" "+" "+=" "-" "-=" "->" ".." "..=" "..." "/="
  ":" "<<" "<<=" "<" "<=" "=" "==" "=>" ">" ">=" ">>" ">>=" "@" "^" "^=" "|" "|=" "||" "?"
] @operator
(unary_expression "!" @operator)
(binary_expression operator: "/" @operator)

(type_arguments "<" @punctuation.bracket ">" @punctuation.bracket)
(type_parameters "<" @punctuation.bracket ">" @punctuation.bracket)
(bracketed_type "<" @punctuation.bracket ">" @punctuation.bracket)
(for_lifetimes "<" @punctuation.bracket ">" @punctuation.bracket)
"#;

/// `true` and `false` are filed under `constant.builtin` by the bundled queries of the
/// languages that use this; Zed colours them as booleans.
const TRUE_FALSE_EXTRA: &str = "[(true) (false)] @boolean\n";

/// As [`TRUE_FALSE_EXTRA`], for C#, whose grammar has one node for both literals.
const BOOLEAN_LITERAL_EXTRA: &str = "(boolean_literal) @boolean\n";

/// As [`TRUE_FALSE_EXTRA`], for PHP.
const PHP_EXTRA: &str = "(boolean) @boolean\n";

/// tree-sitter-python lists its method-call pattern before the pattern that makes every
/// attribute a property, so `obj.method()` comes out as a property. Said again here, after
/// both.
const PYTHON_EXTRA: &str = r#"
[(true) (false)] @boolean
(call
  function: (attribute attribute: (identifier) @function.method))
"#;

/// tree-sitter-javascript treats every capitalised identifier as a constructor (and
/// tree-sitter-typescript as a type), including the name in a function's own definition. A
/// React component is a function with a capitalised name, so its definition is said to be a
/// function again here. References and JSX tags keep the bundled colour, as in Zed.
const JAVASCRIPT_EXTRA: &str = r#"
[(true) (false)] @boolean
(function_declaration name: (identifier) @function)
(function_expression name: (identifier) @function)
(generator_function_declaration name: (identifier) @function)
(variable_declarator
  name: (identifier) @function
  value: [(function_expression) (arrow_function)])
(assignment_expression
  left: (identifier) @function
  right: [(function_expression) (arrow_function)])
"#;

/// tree-sitter-toml captures the whole `key = value` pair as a property instead of the key,
/// which leaves a bare key coloured as a type while a dotted one is a property.
const TOML_EXTRA: &str = "(pair (bare_key) @property)\n";

/// The `)` that ends a `case` pattern is not a bracket. Calling it a delimiter keeps bracket
/// matching from pairing it with an unrelated `(`.
const BASH_EXTRA: &str = "(case_item \")\" @punctuation.delimiter)\n";

/// tree-sitter-kotlin's pattern for a function's name requires the name to be the
/// declaration's first child, so a function with a modifier (`override fun`, `private fun`,
/// `suspend fun`) is not recognised. A declaration has only one identifier of its own.
const KOTLIN_EXTRA: &str = "(function_declaration (simple_identifier) @function)\n";

/// tree-sitter-md injects each piece of HTML on its own, so a closing tag is parsed without
/// the tag it closes and gets no highlight. Neovim's copy of these queries combines the
/// pieces into one HTML document: the tags of one paragraph, and the HTML blocks of one
/// file (a `<details>` and its `</details>` are separate blocks with Markdown between
/// them). These patterns do the same and, coming last, replace the bundled ones.
const MARKDOWN_INLINE_HTML_INJECTION: &str = r#"
((html_tag) @injection.content
 (#set! injection.language "html")
 (#set! injection.combined))
"#;

/// See [`MARKDOWN_INLINE_HTML_INJECTION`].
const MARKDOWN_HTML_BLOCK_INJECTION: &str = r#"
((html_block) @injection.content
 (#set! injection.language "html")
 (#set! injection.combined))
"#;

/// PHP's injection of HTML into the text outside `<?php ... ?>` ships in the grammar's
/// repository as `queries/injections-text.scm` but is not exported by its Rust binding.
const PHP_TEXT_INJECTION: &str = r#"
((text) @injection.content
 (#set! injection.language "html")
 (#set! injection.combined))
"#;

// ---------------------------------------------------------------------------------------
// The grammars.
// ---------------------------------------------------------------------------------------

pub(crate) static RUST: GrammarSpec = GrammarSpec {
    name: "rust",
    language: || tree_sitter_rust::LANGUAGE.into(),
    highlights: &[tree_sitter_rust::HIGHLIGHTS_QUERY, RUST_EXTRA],
    injections: &[tree_sitter_rust::INJECTIONS_QUERY],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::RUST,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static PYTHON: GrammarSpec = GrammarSpec {
    name: "python",
    language: || tree_sitter_python::LANGUAGE.into(),
    highlights: &[tree_sitter_python::HIGHLIGHTS_QUERY, PYTHON_EXTRA],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::PYTHON,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static JAVASCRIPT: GrammarSpec = GrammarSpec {
    name: "javascript",
    language: || tree_sitter_javascript::LANGUAGE.into(),
    highlights: &[
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        JAVASCRIPT_EXTRA,
    ],
    injections: &[tree_sitter_javascript::INJECTIONS_QUERY],
    locals: &[tree_sitter_javascript::LOCALS_QUERY],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::JAVASCRIPT,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

// TypeScript's own queries only cover what it adds to JavaScript, so they go after
// JavaScript's and win where both speak.
pub(crate) static TYPESCRIPT: GrammarSpec = GrammarSpec {
    name: "typescript",
    language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    highlights: &[
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
        JAVASCRIPT_EXTRA,
    ],
    injections: &[tree_sitter_javascript::INJECTIONS_QUERY],
    locals: &[
        tree_sitter_javascript::LOCALS_QUERY,
        tree_sitter_typescript::LOCALS_QUERY,
    ],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::TYPESCRIPT,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static TSX: GrammarSpec = GrammarSpec {
    name: "tsx",
    language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
    highlights: &[
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
        JAVASCRIPT_EXTRA,
    ],
    injections: &[tree_sitter_javascript::INJECTIONS_QUERY],
    locals: &[
        tree_sitter_javascript::LOCALS_QUERY,
        tree_sitter_typescript::LOCALS_QUERY,
    ],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::TYPESCRIPT,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

// The key pattern comes first in this query and is meant to beat the string pattern.
pub(crate) static JSON: GrammarSpec = GrammarSpec {
    name: "json",
    language: || tree_sitter_json::LANGUAGE.into(),
    highlights: &[TRUE_FALSE_EXTRA, tree_sitter_json::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::FirstWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static TOML: GrammarSpec = GrammarSpec {
    name: "toml",
    language: || tree_sitter_toml_ng::LANGUAGE.into(),
    highlights: &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY, TOML_EXTRA],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static YAML: GrammarSpec = GrammarSpec {
    name: "yaml",
    language: || tree_sitter_yaml::LANGUAGE.into(),
    highlights: &[tree_sitter_yaml::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static MARKDOWN: GrammarSpec = GrammarSpec {
    name: "markdown",
    language: || tree_sitter_md::LANGUAGE.into(),
    highlights: &[tree_sitter_md::HIGHLIGHT_QUERY_BLOCK],
    injections: &[
        tree_sitter_md::INJECTION_QUERY_BLOCK,
        MARKDOWN_HTML_BLOCK_INJECTION,
    ],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeNamed,
    outline: outline::MARKDOWN,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static MARKDOWN_INLINE: GrammarSpec = GrammarSpec {
    name: "markdown_inline",
    language: || tree_sitter_md::INLINE_LANGUAGE.into(),
    highlights: &[tree_sitter_md::HIGHLIGHT_QUERY_INLINE],
    injections: &[
        tree_sitter_md::INJECTION_QUERY_INLINE,
        MARKDOWN_INLINE_HTML_INJECTION,
    ],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeNamed,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static BASH: GrammarSpec = GrammarSpec {
    name: "bash",
    language: || tree_sitter_bash::LANGUAGE.into(),
    highlights: &[tree_sitter_bash::HIGHLIGHT_QUERY, BASH_EXTRA],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::BASH,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

// Written specific-first: the call patterns come before `(identifier) @variable`.
pub(crate) static GO: GrammarSpec = GrammarSpec {
    name: "go",
    language: || tree_sitter_go::LANGUAGE.into(),
    highlights: &[TRUE_FALSE_EXTRA, tree_sitter_go::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::FirstWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::GO,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static C: GrammarSpec = GrammarSpec {
    name: "c",
    language: || tree_sitter_c::LANGUAGE.into(),
    highlights: &[tree_sitter_c::HIGHLIGHT_QUERY, TRUE_FALSE_EXTRA],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::C,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

// C++ highlights are C's followed by what C++ adds, as the grammar's tree-sitter.json says.
pub(crate) static CPP: GrammarSpec = GrammarSpec {
    name: "cpp",
    language: || tree_sitter_cpp::LANGUAGE.into(),
    highlights: &[
        tree_sitter_c::HIGHLIGHT_QUERY,
        tree_sitter_cpp::HIGHLIGHT_QUERY,
        TRUE_FALSE_EXTRA,
    ],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::CPP,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static HTML: GrammarSpec = GrammarSpec {
    name: "html",
    language: || tree_sitter_html::LANGUAGE.into(),
    highlights: &[tree_sitter_html::HIGHLIGHTS_QUERY],
    injections: &[tree_sitter_html::INJECTIONS_QUERY],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static CSS: GrammarSpec = GrammarSpec {
    name: "css",
    language: || tree_sitter_css::LANGUAGE.into(),
    highlights: &[tree_sitter_css::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static SWIFT: GrammarSpec = GrammarSpec {
    name: "swift",
    language: || tree_sitter_swift::LANGUAGE.into(),
    highlights: &[tree_sitter_swift::HIGHLIGHTS_QUERY],
    injections: &[tree_sitter_swift::INJECTIONS_QUERY],
    locals: &[tree_sitter_swift::LOCALS_QUERY],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeNamed,
    outline: outline::SWIFT,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static JAVA: GrammarSpec = GrammarSpec {
    name: "java",
    language: || tree_sitter_java::LANGUAGE.into(),
    highlights: &[tree_sitter_java::HIGHLIGHTS_QUERY, TRUE_FALSE_EXTRA],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::JAVA,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static LUA: GrammarSpec = GrammarSpec {
    name: "lua",
    language: || tree_sitter_lua::LANGUAGE.into(),
    highlights: &[tree_sitter_lua::HIGHLIGHTS_QUERY],
    injections: &[tree_sitter_lua::INJECTIONS_QUERY],
    locals: &[tree_sitter_lua::LOCALS_QUERY],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeNamed,
    outline: outline::LUA,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static DOCKERFILE: GrammarSpec = GrammarSpec {
    name: "dockerfile",
    language: || tree_sitter_dockerfile_orchard::LANGUAGE.into(),
    highlights: &[tree_sitter_dockerfile_orchard::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static SQL: GrammarSpec = GrammarSpec {
    name: "sql",
    language: || tree_sitter_sequel::LANGUAGE.into(),
    highlights: &[tree_sitter_sequel::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static RUBY: GrammarSpec = GrammarSpec {
    name: "ruby",
    language: || tree_sitter_ruby::LANGUAGE.into(),
    highlights: &[tree_sitter_ruby::HIGHLIGHTS_QUERY, TRUE_FALSE_EXTRA],
    injections: &[],
    locals: &[tree_sitter_ruby::LOCALS_QUERY],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::RUBY,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static KOTLIN: GrammarSpec = GrammarSpec {
    name: "kotlin",
    language: || tree_sitter_kotlin_sg::LANGUAGE.into(),
    highlights: &[tree_sitter_kotlin_sg::HIGHLIGHTS_QUERY, KOTLIN_EXTRA],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeNamed,
    outline: outline::KOTLIN,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static DIFF: GrammarSpec = GrammarSpec {
    name: "diff",
    language: || tree_sitter_diff::LANGUAGE.into(),
    highlights: &[tree_sitter_diff::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static MAKE: GrammarSpec = GrammarSpec {
    name: "make",
    language: || tree_sitter_make::LANGUAGE.into(),
    highlights: &[tree_sitter_make::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeNamed,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

// Written specific-first: the builtin and function patterns come before
// `(identifier) @variable`.
pub(crate) static NIX: GrammarSpec = GrammarSpec {
    name: "nix",
    language: || tree_sitter_nix::LANGUAGE.into(),
    highlights: &[tree_sitter_nix::HIGHLIGHTS_QUERY],
    injections: &[tree_sitter_nix::INJECTIONS_QUERY],
    locals: &[],
    precedence: Precedence::FirstWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static ZIG: GrammarSpec = GrammarSpec {
    name: "zig",
    language: || tree_sitter_zig::LANGUAGE.into(),
    highlights: &[tree_sitter_zig::HIGHLIGHTS_QUERY],
    injections: &[tree_sitter_zig::INJECTIONS_QUERY],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeNamed,
    outline: outline::ZIG,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static PHP: GrammarSpec = GrammarSpec {
    name: "php",
    language: || tree_sitter_php::LANGUAGE_PHP.into(),
    highlights: &[tree_sitter_php::HIGHLIGHTS_QUERY, PHP_EXTRA],
    injections: &[tree_sitter_php::INJECTIONS_QUERY, PHP_TEXT_INJECTION],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::PHP,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static CSHARP: GrammarSpec = GrammarSpec {
    name: "csharp",
    language: || tree_sitter_c_sharp::LANGUAGE.into(),
    highlights: &[tree_sitter_c_sharp::HIGHLIGHTS_QUERY, BOOLEAN_LITERAL_EXTRA],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::CSHARP,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static DART: GrammarSpec = GrammarSpec {
    name: "dart",
    language: || tree_sitter_dart::LANGUAGE.into(),
    highlights: &[tree_sitter_dart::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[tree_sitter_dart::LOCALS_QUERY],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::DART,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static GOMOD: GrammarSpec = GrammarSpec {
    name: "gomod",
    language: || tree_sitter_gomod_orchard::LANGUAGE.into(),
    highlights: &[tree_sitter_gomod_orchard::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static GIT_COMMIT: GrammarSpec = GrammarSpec {
    name: "gitcommit",
    language: || tree_sitter_gitcommit::LANGUAGE.into(),
    highlights: &[tree_sitter_gitcommit::HIGHLIGHTS_QUERY],
    injections: &[tree_sitter_gitcommit::INJECTIONS_QUERY],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeNamed,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

pub(crate) static REGEX: GrammarSpec = GrammarSpec {
    name: "regex",
    language: || tree_sitter_regex::LANGUAGE.into(),
    highlights: &[tree_sitter_regex::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::LastWins,
    children: InjectedChildren::ExcludeAll,
    outline: &[],
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

/// A grammar no language uses, so a test can watch it being loaded without another test
/// loading it first.
#[cfg(test)]
pub(crate) static UNTOUCHED: GrammarSpec = GrammarSpec {
    name: "untouched",
    language: || tree_sitter_json::LANGUAGE.into(),
    highlights: &[tree_sitter_json::HIGHLIGHTS_QUERY],
    injections: &[],
    locals: &[],
    precedence: Precedence::FirstWins,
    children: InjectedChildren::ExcludeAll,
    outline: outline::RUST,
    loaded: OnceLock::new(),
    outline_loaded: OnceLock::new(),
};

/// Every grammar, including the ones that only exist to be injected.
#[cfg(test)]
pub(crate) static ALL: &[&GrammarSpec] = &[
    &RUST,
    &PYTHON,
    &JAVASCRIPT,
    &TYPESCRIPT,
    &TSX,
    &JSON,
    &TOML,
    &YAML,
    &MARKDOWN,
    &MARKDOWN_INLINE,
    &BASH,
    &GO,
    &C,
    &CPP,
    &HTML,
    &CSS,
    &SWIFT,
    &JAVA,
    &LUA,
    &DOCKERFILE,
    &SQL,
    &RUBY,
    &KOTLIN,
    &DIFF,
    &MAKE,
    &NIX,
    &ZIG,
    &PHP,
    &CSHARP,
    &DART,
    &GOMOD,
    &GIT_COMMIT,
    &REGEX,
];

#[cfg(test)]
mod tests {
    use super::*;
    use bakedit_core::Theme;
    use std::collections::BTreeSet;

    /// Captures the built-in theme leaves in the editor's foreground colour on purpose:
    ///
    /// * `embedded` and `none` mark code inside a string or a fence that is *not* string:
    ///   they exist to stop the surrounding colour, so they must not have one.
    /// * `label` (lifetimes, YAML anchors, goto labels) and `link_text` (the text of a
    ///   Markdown link, a branch name in a commit message) are in Zed's vocabulary but the
    ///   theme does not colour them.
    /// * `error` and `text.*` are tree-sitter-make's: a stray `/` in an automatic variable
    ///   and the message of `$(error ...)`, `$(warning ...)` and `$(info ...)`.
    const UNSTYLED: &[&str] = &[
        "embedded",
        "none",
        "label",
        "link_text",
        "error",
        "text.danger",
        "text.warning",
        "text.note",
    ];

    /// Captures in the bundled queries that are not highlights at all.
    const NOT_HIGHLIGHTS: &[&str] = &[
        "_class",
        "_function",
        "clean",
        "spell",
        "nospell",
        "local.scope",
        "local.definition",
        "local.definition.function",
        "local.definition.import",
        "local.reference",
    ];

    /// Every `(grammar, raw capture name)` in the highlight and locals queries.
    fn raw_captures() -> Vec<(&'static str, &'static str)> {
        let mut captures = Vec::new();
        for spec in ALL {
            let grammar = load(spec).unwrap_or_else(|error| panic!("{error}"));
            captures.extend(
                grammar
                    .query
                    .capture_names()
                    .iter()
                    .map(|name| (spec.name, *name)),
            );
        }
        captures
    }

    #[test]
    fn every_capture_has_a_style_in_the_builtin_theme_or_is_deliberately_unstyled() {
        let theme = Theme::builtin();
        let mut unexpected = BTreeSet::new();
        for (grammar, raw) in raw_captures() {
            let Some(name) = capture::normalize(raw) else {
                continue;
            };
            if theme.syntax(&name).is_none() && !UNSTYLED.contains(&name.as_str()) {
                unexpected.insert(format!("{grammar}: @{raw} -> {name}"));
            }
        }
        assert!(
            unexpected.is_empty(),
            "captures the theme cannot draw: {unexpected:#?}"
        );
    }

    #[test]
    fn only_bookkeeping_captures_are_dropped() {
        let mut unexpected = BTreeSet::new();
        for (grammar, raw) in raw_captures() {
            if capture::normalize(raw).is_none() && !NOT_HIGHLIGHTS.contains(&raw) {
                unexpected.insert(format!("{grammar}: @{raw}"));
            }
        }
        assert!(
            unexpected.is_empty(),
            "captures dropped without a reason: {unexpected:#?}"
        );
    }

    #[test]
    fn the_exception_lists_have_no_stale_entries() {
        let theme = Theme::builtin();
        let raw: BTreeSet<&str> = raw_captures().into_iter().map(|(_, name)| name).collect();
        let normalized: BTreeSet<String> = raw
            .iter()
            .filter_map(|name| capture::normalize(name))
            .collect();
        for name in UNSTYLED {
            assert!(
                normalized.contains(*name),
                "no grammar produces `{name}` any more"
            );
            assert!(
                theme.syntax(name).is_none(),
                "the theme now styles `{name}`"
            );
        }
        for name in NOT_HIGHLIGHTS {
            assert!(raw.contains(name), "no grammar captures `@{name}` any more");
        }
    }

    #[test]
    fn the_theme_vocabulary_is_what_the_grammars_speak() {
        // Every capture the built-in theme styles is produced by some grammar, directly or
        // as the root of a dotted name, so the theme and the normalisation agree.
        let theme = Theme::builtin();
        let produced: BTreeSet<String> = raw_captures()
            .into_iter()
            .filter_map(|(_, raw)| capture::normalize(raw))
            .collect();
        for styled in theme.syntax_captures() {
            let used = produced
                .iter()
                .any(|name| name == styled || name.starts_with(&format!("{styled}.")));
            assert!(
                used,
                "no grammar produces anything the theme's `{styled}` would colour"
            );
        }
    }

    #[test]
    fn a_language_lists_its_normalised_captures() {
        let rust = crate::language_by_id("rust").unwrap().capture_names();
        assert!(rust.is_sorted());
        for expected in [
            "keyword",
            "string",
            "string.escape",
            "comment",
            "function",
            "function.macro",
            "type",
        ] {
            assert!(
                rust.contains(&expected),
                "{expected} is missing from {rust:?}"
            );
        }
        assert!(!rust.contains(&"escape") && !rust.contains(&"injection.content"));
        let markdown = crate::language_by_id("markdown").unwrap().capture_names();
        assert!(markdown.contains(&"title") && !markdown.contains(&"text.title"));
    }

    #[test]
    fn locals_patterns_come_first_and_are_counted() {
        let javascript = load(&JAVASCRIPT).unwrap();
        assert!(javascript.locals_patterns > 0);
        assert!(javascript.locals_patterns < javascript.query.pattern_count());
        assert_eq!(javascript.patterns.len(), javascript.query.pattern_count());
        let scope = javascript
            .query
            .capture_index_for_name("local.scope")
            .unwrap() as usize;
        assert_eq!(javascript.local_roles[scope], LocalRole::Scope);
        assert_eq!(javascript.capture_ids[scope], None);
        assert_eq!(load(&RUST).unwrap().locals_patterns, 0);
        // `(#is-not? local)` marks the builtin patterns.
        assert!(javascript.patterns.iter().any(|pattern| pattern.non_local));
        // Ruby's methods do not see the locals of the code around them.
        let ruby = load(&RUBY).unwrap();
        assert!(ruby.patterns[..ruby.locals_patterns]
            .iter()
            .any(|pattern| !pattern.scope_inherits));
    }

    #[test]
    fn injection_patterns_know_their_language_and_flags() {
        let markdown = load(&MARKDOWN).unwrap();
        let injections = markdown.injections.as_ref().unwrap();
        assert!(injections.language_capture.is_some());
        let named = |pattern: &InjectionPattern| match pattern.language {
            Some(FixedLanguage::Named(Some(spec))) => Some(spec.name),
            _ => None,
        };
        let names: Vec<&str> = injections.patterns.iter().filter_map(named).collect();
        for expected in ["markdown_inline", "html", "yaml", "toml"] {
            assert!(names.contains(&expected), "{expected} is not in {names:?}");
        }
        let rust = load(&RUST).unwrap();
        let rust_injections = rust.injections.as_ref().unwrap();
        assert!(rust_injections
            .patterns
            .iter()
            .all(|pattern| pattern.include_children));
        assert!(rust_injections
            .patterns
            .iter()
            .all(|pattern| named(pattern) == Some("rust")));
        let php = load(&PHP).unwrap();
        assert!(php
            .injections
            .as_ref()
            .unwrap()
            .patterns
            .iter()
            .any(|pattern| pattern.combined));
        // A language this crate does not have resolves to nothing, not to an error.
        let zig = load(&ZIG).unwrap();
        let comment = &zig.injections.as_ref().unwrap().patterns[0];
        assert!(matches!(comment.language, Some(FixedLanguage::Named(None))));
    }

    #[test]
    fn a_predicate_that_is_not_implemented_disables_its_pattern() {
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let source = r#"
((identifier) @keyword (#has-ancestor? @keyword function_item))
((identifier) @string (#lua-match? @string "^%l+$"))
((identifier) @comment (#not-lua-match? @comment "^%u"))
((identifier) @number (#lua-match? @number "%b()"))
((identifier) @type (#offset! @type 0 1 0 -1))
(identifier) @variable
"#;
        let query = Query::new(&language, source).unwrap();
        let predicates: Vec<Option<usize>> = (0..query.pattern_count())
            .map(|pattern| custom_predicates(&query, pattern).map(|predicates| predicates.len()))
            .collect();
        // Unknown test, two Lua patterns, an untranslatable Lua pattern, a directive, nothing.
        assert_eq!(predicates, [None, Some(1), Some(1), None, Some(0), Some(0)]);
        let negated = custom_predicates(&query, 2).unwrap();
        assert!(matches!(
            negated[0],
            Predicate::LuaMatch { negated: true, .. }
        ));
    }
}
