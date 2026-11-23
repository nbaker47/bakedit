//! The outline of a file: its functions, types, headings and other named parts.
//!
//! Each language that has an outline describes it as a list of [`OutlineRule`]s. A rule is a
//! tree-sitter pattern that captures the whole definition as `@item` and its name as
//! `@name`, and says what kind of symbol that is. Rules go from general to specific: when
//! two match the same node the later one names its kind, which is how a function inside an
//! `impl` becomes a method.
//!
//! Symbols nest by containment: a symbol's depth is the number of other symbols whose
//! `@item` encloses its own. Markdown headings are the exception, because a heading does not
//! contain the text under it; they nest by level instead.

use tree_sitter::{
    ParseOptions, ParseState, Parser, Query, QueryCursor, QueryCursorOptions, QueryCursorState,
    StreamingIterator,
};

use crate::highlight::{band_roots, parse_time_allowed, Watchdog, BAND_DEPTH, MATCH_LIMIT};
use crate::language::Language;
use crate::lines::{lines_of_offsets, LineBreaks};

/// What a [`Symbol`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    /// A free function, or a function declaration.
    Function,
    /// A function that belongs to a type, trait, class or interface.
    Method,
    /// A struct.
    Struct,
    /// A class, including Swift actors and Kotlin objects.
    Class,
    /// An enum.
    Enum,
    /// One case of an enum.
    Variant,
    /// A trait.
    Trait,
    /// An interface or protocol.
    Interface,
    /// A Rust `impl` block or a Swift extension.
    Impl,
    /// A module.
    Module,
    /// A namespace or package.
    Namespace,
    /// A constant.
    Constant,
    /// A variable or static.
    Variable,
    /// A field or property of a type.
    Field,
    /// A type alias or typedef.
    Type,
    /// A macro.
    Macro,
    /// A union.
    Union,
    /// A Markdown heading.
    Heading,
}

/// One entry of a file's outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The symbol's name as written, with runs of white space collapsed. An `impl` block is
    /// named for its type, as `Trait for Type` when it implements a trait.
    pub name: String,
    /// What the symbol is.
    pub kind: SymbolKind,
    /// The 0-based line the name is on.
    pub line: usize,
    /// How many other symbols enclose this one: 0 at the top level, 1 for a method in an
    /// `impl` or a heading under another heading.
    pub depth: usize,
    /// Byte offset of the first byte of the name.
    pub byte: usize,
}

/// One kind of symbol in a language: a pattern capturing `@item` and `@name`.
pub(crate) struct OutlineRule {
    pub kind: SymbolKind,
    /// For a heading, its level from 1; 0 for everything that nests by containment.
    pub level: u8,
    pub pattern: &'static str,
}

const fn rule(kind: SymbolKind, pattern: &'static str) -> OutlineRule {
    OutlineRule {
        kind,
        level: 0,
        pattern,
    }
}

const fn heading(level: u8, pattern: &'static str) -> OutlineRule {
    OutlineRule {
        kind: SymbolKind::Heading,
        level,
        pattern,
    }
}

/// A language's rules compiled into one query.
pub(crate) struct OutlineQuery {
    query: Query,
    /// The kind and heading level each pattern of `query` stands for.
    kinds: Vec<(SymbolKind, u8)>,
    item: u32,
    name: u32,
    /// `@trait`, the trait of a Rust impl.
    qualifier: Option<u32>,
}

impl OutlineQuery {
    /// Compile `rules` for `language`. `None` when there are no rules, or they do not
    /// compile, or a rule is more than one pattern (the kinds would no longer line up).
    pub(crate) fn compile(
        language: &tree_sitter::Language,
        rules: &[OutlineRule],
    ) -> Option<OutlineQuery> {
        if rules.is_empty() {
            return None;
        }
        let source: String = rules
            .iter()
            .map(|rule| format!("{}\n", rule.pattern))
            .collect();
        let query = Query::new(language, &source).ok()?;
        if query.pattern_count() != rules.len() {
            return None;
        }
        Some(OutlineQuery {
            item: query.capture_index_for_name("item")?,
            name: query.capture_index_for_name("name")?,
            qualifier: query.capture_index_for_name("trait"),
            kinds: rules.iter().map(|rule| (rule.kind, rule.level)).collect(),
            query,
        })
    }
}

/// A symbol as matched, before nesting and line numbers are worked out.
struct Found {
    item_start: usize,
    item_end: usize,
    pattern: usize,
    name: String,
    name_start: usize,
}

/// The outline of `text`, in the order the symbols appear.
///
/// Empty when the language has no grammar or no outline rules, and for text too large to
/// parse. Symbols in the middle of a syntax error may be missing, but an error elsewhere in
/// the file does not hide the rest. Lines are counted the default way
/// ([`LineBreaks::Unicode`]); convert [`Symbol::byte`] yourself if the buffer counts
/// otherwise.
///
/// This parses the text, so it costs about as much as highlighting it: a few milliseconds
/// for an ordinary file, tens for a very large one. On the rare input a grammar chokes on it
/// gives up after two seconds (longer for a text of megabytes) and returns what it has,
/// which may be nothing; call it off the UI thread if that matters.
pub fn outline(language: &Language, text: &str) -> Vec<Symbol> {
    let Some(spec) = language.grammar else {
        return Vec::new();
    };
    let Some(rules) = spec.outline_query() else {
        return Vec::new();
    };
    let Ok(grammar) = crate::grammar::load(spec) else {
        return Vec::new();
    };
    if text.len() > u32::MAX as usize {
        return Vec::new();
    }
    // The same time limit as a highlighting parse, shared by the parse and the query.
    let watchdog = Watchdog::new(None, parse_time_allowed(text.len()));
    let mut parser = Parser::new();
    if parser.set_language(&grammar.language).is_err() {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let mut on_parse_progress = |_: &ParseState| watchdog.check();
    let parse_options = ParseOptions::new().progress_callback(&mut on_parse_progress);
    let tree = parser.parse_with_options(
        &mut |offset, _| &bytes[offset.min(bytes.len())..],
        None,
        Some(parse_options),
    );
    let Some(tree) = tree else {
        return Vec::new();
    };

    let mut found: Vec<Found> = Vec::new();
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(MATCH_LIMIT);
    cursor.set_max_start_depth(Some(BAND_DEPTH));
    for band_root in band_roots(tree.root_node()) {
        let mut on_query_progress = |_: &QueryCursorState| watchdog.check();
        let query_options = QueryCursorOptions::new().progress_callback(&mut on_query_progress);
        let mut matches =
            cursor.matches_with_options(&rules.query, band_root, bytes, query_options);
        while let Some(matched) = matches.next() {
            let node_of = |capture: u32| matched.nodes_for_capture_index(capture).next();
            let (Some(item), Some(name)) = (node_of(rules.item), node_of(rules.name)) else {
                continue;
            };
            let mut label = collapse_whitespace(text.get(name.byte_range()).unwrap_or(""));
            if let Some(qualifier) = rules.qualifier.and_then(node_of) {
                let qualifier = collapse_whitespace(text.get(qualifier.byte_range()).unwrap_or(""));
                label = format!("{qualifier} for {label}");
            }
            if label.is_empty() {
                continue;
            }
            found.push(Found {
                item_start: item.start_byte(),
                item_end: item.end_byte(),
                pattern: matched.pattern_index,
                name: label,
                name_start: name.start_byte(),
            });
        }
    }

    // Document order, an enclosing item before what it contains. When several rules name
    // the same thing the most specific (the last) comes first and the rest are dropped. One
    // item can still give several symbols: `x, y int` declares two fields.
    found.sort_by(|a, b| {
        (a.item_start, b.item_end, a.name_start, b.pattern).cmp(&(
            b.item_start,
            a.item_end,
            b.name_start,
            a.pattern,
        ))
    });
    found.dedup_by(|later, earlier| {
        (later.item_start, later.item_end, later.name_start)
            == (earlier.item_start, earlier.item_end, earlier.name_start)
    });

    let offsets: Vec<usize> = found.iter().map(|symbol| symbol.name_start).collect();
    let mut order: Vec<usize> = (0..found.len()).collect();
    order.sort_by_key(|&index| offsets[index]);
    let sorted_offsets: Vec<usize> = order.iter().map(|&index| offsets[index]).collect();
    let sorted_lines = lines_of_offsets(text, &sorted_offsets, LineBreaks::default());
    let mut lines = vec![0; found.len()];
    for (position, &index) in order.iter().enumerate() {
        lines[index] = sorted_lines[position];
    }

    let mut symbols = Vec::with_capacity(found.len());
    // The ends of the items around the current symbol, and the levels of the headings above
    // the current heading.
    let mut enclosing: Vec<(usize, usize)> = Vec::new();
    let mut heading_levels: Vec<u8> = Vec::new();
    for (index, symbol) in found.into_iter().enumerate() {
        let (kind, level) = rules.kinds[symbol.pattern];
        let depth = if level > 0 {
            while heading_levels.last().is_some_and(|&above| above >= level) {
                heading_levels.pop();
            }
            heading_levels.push(level);
            heading_levels.len() - 1
        } else {
            // An item that is over, or the very same item naming a second symbol, does not
            // enclose this one.
            let item = (symbol.item_start, symbol.item_end);
            while enclosing
                .last()
                .is_some_and(|&outer| outer.1 <= item.0 || outer == item)
            {
                enclosing.pop();
            }
            enclosing.push(item);
            enclosing.len() - 1
        };
        symbols.push(Symbol {
            name: symbol.name,
            kind,
            line: lines[index],
            depth,
            byte: symbol.name_start,
        });
    }
    symbols
}

/// Trim, and turn every run of white space (a signature split over lines) into one space.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

use SymbolKind::*;

pub(crate) const RUST: &[OutlineRule] = &[
    rule(Function, "(function_item name: (identifier) @name) @item"),
    rule(Function, "(function_signature_item name: (identifier) @name) @item"),
    rule(Method, "(impl_item body: (declaration_list (function_item name: (identifier) @name) @item))"),
    rule(Method, "(trait_item body: (declaration_list (function_item name: (identifier) @name) @item))"),
    rule(
        Method,
        "(trait_item body: (declaration_list (function_signature_item name: (identifier) @name) @item))",
    ),
    rule(Struct, "(struct_item name: (type_identifier) @name) @item"),
    rule(Enum, "(enum_item name: (type_identifier) @name) @item"),
    rule(Variant, "(enum_variant name: (identifier) @name) @item"),
    rule(Union, "(union_item name: (type_identifier) @name) @item"),
    rule(Trait, "(trait_item name: (type_identifier) @name) @item"),
    rule(Impl, "(impl_item !trait type: (_) @name) @item"),
    rule(Impl, "(impl_item trait: (_) @trait type: (_) @name) @item"),
    rule(Module, "(mod_item name: (identifier) @name) @item"),
    rule(Constant, "(const_item name: (identifier) @name) @item"),
    rule(Variable, "(static_item name: (identifier) @name) @item"),
    rule(Type, "(type_item name: (type_identifier) @name) @item"),
    rule(Type, "(associated_type name: (type_identifier) @name) @item"),
    rule(Macro, "(macro_definition name: (identifier) @name) @item"),
    rule(Field, "(field_declaration name: (field_identifier) @name) @item"),
];

pub(crate) const PYTHON: &[OutlineRule] = &[
    rule(Variable, "(module (expression_statement (assignment left: (identifier) @name) @item))"),
    rule(Class, "(class_definition name: (identifier) @name) @item"),
    rule(Function, "(function_definition name: (identifier) @name) @item"),
    rule(Method, "(class_definition body: (block (function_definition name: (identifier) @name) @item))"),
    rule(
        Method,
        "(class_definition body: (block (decorated_definition definition: (function_definition name: (identifier) @name) @item)))",
    ),
];

pub(crate) const JAVASCRIPT: &[OutlineRule] = &[
    rule(Variable, "(program (lexical_declaration (variable_declarator name: (identifier) @name) @item))"),
    rule(Variable, "(program (variable_declaration (variable_declarator name: (identifier) @name) @item))"),
    rule(
        Variable,
        "(program (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name) @item)))",
    ),
    rule(Function, "(function_declaration name: (identifier) @name) @item"),
    rule(Function, "(generator_function_declaration name: (identifier) @name) @item"),
    rule(
        Function,
        "(variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @item",
    ),
    rule(Class, "(class_declaration name: (_) @name) @item"),
    rule(Method, "(method_definition name: (_) @name) @item"),
    rule(Field, "(field_definition property: (_) @name) @item"),
];

pub(crate) const TYPESCRIPT: &[OutlineRule] = &[
    rule(Variable, "(program (lexical_declaration (variable_declarator name: (identifier) @name) @item))"),
    rule(Variable, "(program (variable_declaration (variable_declarator name: (identifier) @name) @item))"),
    rule(
        Variable,
        "(program (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name) @item)))",
    ),
    rule(Function, "(function_declaration name: (identifier) @name) @item"),
    rule(Function, "(generator_function_declaration name: (identifier) @name) @item"),
    rule(Function, "(function_signature name: (identifier) @name) @item"),
    rule(
        Function,
        "(variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @item",
    ),
    rule(Class, "(class_declaration name: (_) @name) @item"),
    rule(Class, "(abstract_class_declaration name: (_) @name) @item"),
    rule(Interface, "(interface_declaration name: (type_identifier) @name) @item"),
    rule(Type, "(type_alias_declaration name: (type_identifier) @name) @item"),
    rule(Enum, "(enum_declaration name: (identifier) @name) @item"),
    rule(Module, "(module name: (_) @name) @item"),
    rule(Module, "(internal_module name: (_) @name) @item"),
    rule(Method, "(method_definition name: (_) @name) @item"),
    rule(Method, "(method_signature name: (_) @name) @item"),
    rule(Method, "(abstract_method_signature name: (_) @name) @item"),
    rule(Field, "(public_field_definition name: (_) @name) @item"),
    rule(Field, "(property_signature name: (_) @name) @item"),
];

pub(crate) const GO: &[OutlineRule] = &[
    rule(
        Function,
        "(function_declaration name: (identifier) @name) @item",
    ),
    rule(
        Method,
        "(method_declaration name: (field_identifier) @name) @item",
    ),
    rule(
        Type,
        "(type_declaration (type_spec name: (type_identifier) @name) @item)",
    ),
    rule(
        Type,
        "(type_declaration (type_alias name: (type_identifier) @name) @item)",
    ),
    rule(
        Struct,
        "(type_declaration (type_spec name: (type_identifier) @name type: (struct_type)) @item)",
    ),
    rule(
        Interface,
        "(type_declaration (type_spec name: (type_identifier) @name type: (interface_type)) @item)",
    ),
    rule(
        Constant,
        "(source_file (const_declaration (const_spec name: (identifier) @name) @item))",
    ),
    rule(
        Variable,
        "(source_file (var_declaration (var_spec name: (identifier) @name) @item))",
    ),
    rule(
        Field,
        "(field_declaration name: (field_identifier) @name) @item",
    ),
    rule(Method, "(method_elem name: (field_identifier) @name) @item"),
];

pub(crate) const C: &[OutlineRule] = &[
    rule(Variable, "(translation_unit (declaration declarator: (identifier) @name) @item)"),
    rule(
        Variable,
        "(translation_unit (declaration declarator: (init_declarator declarator: (identifier) @name)) @item)",
    ),
    rule(Function, "(declaration declarator: (function_declarator declarator: (identifier) @name)) @item"),
    rule(Function, "(function_definition declarator: (function_declarator declarator: (identifier) @name)) @item"),
    rule(
        Function,
        "(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @item",
    ),
    rule(
        Function,
        "(function_definition declarator: (pointer_declarator declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name)))) @item",
    ),
    rule(Struct, "(struct_specifier name: (type_identifier) @name body: (_)) @item"),
    rule(Union, "(union_specifier name: (type_identifier) @name body: (_)) @item"),
    rule(Enum, "(enum_specifier name: (type_identifier) @name body: (_)) @item"),
    rule(Variant, "(enumerator name: (identifier) @name) @item"),
    rule(Type, "(type_definition declarator: (type_identifier) @name) @item"),
    rule(Macro, "(preproc_def name: (identifier) @name) @item"),
    rule(Macro, "(preproc_function_def name: (identifier) @name) @item"),
    rule(Field, "(field_declaration declarator: (field_identifier) @name) @item"),
];

pub(crate) const CPP: &[OutlineRule] = &[
    rule(Variable, "(translation_unit (declaration declarator: (identifier) @name) @item)"),
    rule(
        Variable,
        "(translation_unit (declaration declarator: (init_declarator declarator: (identifier) @name)) @item)",
    ),
    rule(Function, "(declaration declarator: (function_declarator declarator: (identifier) @name)) @item"),
    rule(Function, "(function_definition declarator: (function_declarator declarator: (identifier) @name)) @item"),
    rule(
        Function,
        "(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @item",
    ),
    rule(
        Function,
        "(function_definition declarator: (reference_declarator (function_declarator declarator: (identifier) @name))) @item",
    ),
    rule(
        Method,
        "(field_declaration_list (declaration declarator: (function_declarator declarator: (identifier) @name)) @item)",
    ),
    rule(
        Method,
        "(field_declaration_list (function_definition declarator: (function_declarator declarator: (identifier) @name)) @item)",
    ),
    rule(
        Method,
        "(function_definition declarator: (function_declarator declarator: [(field_identifier) (qualified_identifier) (destructor_name) (operator_name)] @name)) @item",
    ),
    rule(
        Method,
        "(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: [(field_identifier) (qualified_identifier)] @name))) @item",
    ),
    rule(
        Method,
        "(function_definition declarator: (reference_declarator (function_declarator declarator: [(field_identifier) (qualified_identifier) (operator_name)] @name))) @item",
    ),
    rule(
        Method,
        "(field_declaration declarator: (function_declarator declarator: [(field_identifier) (destructor_name) (operator_name)] @name)) @item",
    ),
    rule(
        Method,
        "(declaration declarator: (function_declarator declarator: [(field_identifier) (qualified_identifier) (destructor_name)] @name)) @item",
    ),
    rule(Struct, "(struct_specifier name: (_) @name body: (_)) @item"),
    rule(Union, "(union_specifier name: (_) @name body: (_)) @item"),
    rule(Enum, "(enum_specifier name: (_) @name body: (_)) @item"),
    rule(Variant, "(enumerator name: (identifier) @name) @item"),
    rule(Class, "(class_specifier name: (_) @name body: (_)) @item"),
    rule(Namespace, "(namespace_definition name: (_) @name) @item"),
    rule(Type, "(type_definition declarator: (type_identifier) @name) @item"),
    rule(Type, "(alias_declaration name: (type_identifier) @name) @item"),
    rule(Macro, "(preproc_def name: (identifier) @name) @item"),
    rule(Macro, "(preproc_function_def name: (identifier) @name) @item"),
    rule(Field, "(field_declaration declarator: (field_identifier) @name) @item"),
];

pub(crate) const SWIFT: &[OutlineRule] = &[
    rule(
        Variable,
        "(source_file (property_declaration (pattern (simple_identifier) @name)) @item)",
    ),
    rule(
        Class,
        "(class_declaration declaration_kind: \"class\" name: (_) @name) @item",
    ),
    rule(
        Class,
        "(class_declaration declaration_kind: \"actor\" name: (_) @name) @item",
    ),
    rule(
        Struct,
        "(class_declaration declaration_kind: \"struct\" name: (_) @name) @item",
    ),
    rule(
        Enum,
        "(class_declaration declaration_kind: \"enum\" name: (_) @name) @item",
    ),
    rule(
        Impl,
        "(class_declaration declaration_kind: \"extension\" name: (_) @name) @item",
    ),
    rule(
        Interface,
        "(protocol_declaration name: (type_identifier) @name) @item",
    ),
    rule(
        Function,
        "(function_declaration name: (simple_identifier) @name) @item",
    ),
    rule(
        Method,
        "(class_body (function_declaration name: (simple_identifier) @name) @item)",
    ),
    rule(
        Method,
        "(enum_class_body (function_declaration name: (simple_identifier) @name) @item)",
    ),
    rule(
        Method,
        "(protocol_function_declaration name: (simple_identifier) @name) @item",
    ),
    rule(Method, "(init_declaration \"init\" @name) @item"),
    rule(Method, "(deinit_declaration \"deinit\" @name) @item"),
    rule(
        Field,
        "(class_body (property_declaration (pattern (simple_identifier) @name)) @item)",
    ),
    rule(
        Field,
        "(enum_class_body (property_declaration (pattern (simple_identifier) @name)) @item)",
    ),
    rule(
        Field,
        "(protocol_property_declaration (pattern (simple_identifier) @name)) @item",
    ),
    rule(
        Variant,
        "(enum_entry name: (simple_identifier) @name) @item",
    ),
    rule(
        Type,
        "(typealias_declaration name: (type_identifier) @name) @item",
    ),
];

pub(crate) const MARKDOWN: &[OutlineRule] = &[
    heading(
        1,
        "(atx_heading (atx_h1_marker) heading_content: (inline) @name) @item",
    ),
    heading(
        2,
        "(atx_heading (atx_h2_marker) heading_content: (inline) @name) @item",
    ),
    heading(
        3,
        "(atx_heading (atx_h3_marker) heading_content: (inline) @name) @item",
    ),
    heading(
        4,
        "(atx_heading (atx_h4_marker) heading_content: (inline) @name) @item",
    ),
    heading(
        5,
        "(atx_heading (atx_h5_marker) heading_content: (inline) @name) @item",
    ),
    heading(
        6,
        "(atx_heading (atx_h6_marker) heading_content: (inline) @name) @item",
    ),
    heading(
        1,
        "(setext_heading heading_content: (paragraph) @name (setext_h1_underline)) @item",
    ),
    heading(
        2,
        "(setext_heading heading_content: (paragraph) @name (setext_h2_underline)) @item",
    ),
];

pub(crate) const BASH: &[OutlineRule] = &[rule(
    Function,
    "(function_definition name: (word) @name) @item",
)];

pub(crate) const JAVA: &[OutlineRule] = &[
    rule(Class, "(class_declaration name: (identifier) @name) @item"),
    rule(Class, "(record_declaration name: (identifier) @name) @item"),
    rule(
        Interface,
        "(interface_declaration name: (identifier) @name) @item",
    ),
    rule(
        Interface,
        "(annotation_type_declaration name: (identifier) @name) @item",
    ),
    rule(Enum, "(enum_declaration name: (identifier) @name) @item"),
    rule(Variant, "(enum_constant name: (identifier) @name) @item"),
    rule(
        Method,
        "(method_declaration name: (identifier) @name) @item",
    ),
    rule(
        Method,
        "(constructor_declaration name: (identifier) @name) @item",
    ),
    rule(
        Field,
        "(field_declaration declarator: (variable_declarator name: (identifier) @name)) @item",
    ),
];

pub(crate) const RUBY: &[OutlineRule] = &[
    rule(Class, "(class name: (_) @name) @item"),
    rule(Module, "(module name: (_) @name) @item"),
    rule(Method, "(method name: (_) @name) @item"),
    rule(Method, "(singleton_method name: (_) @name) @item"),
];

pub(crate) const LUA: &[OutlineRule] = &[rule(
    Function,
    "(function_declaration name: (_) @name) @item",
)];

pub(crate) const PHP: &[OutlineRule] = &[
    rule(
        Namespace,
        "(namespace_definition name: (namespace_name) @name) @item",
    ),
    rule(Class, "(class_declaration name: (name) @name) @item"),
    rule(
        Interface,
        "(interface_declaration name: (name) @name) @item",
    ),
    rule(Trait, "(trait_declaration name: (name) @name) @item"),
    rule(Enum, "(enum_declaration name: (name) @name) @item"),
    rule(Variant, "(enum_case name: (name) @name) @item"),
    rule(Function, "(function_definition name: (name) @name) @item"),
    rule(Method, "(method_declaration name: (name) @name) @item"),
];

pub(crate) const CSHARP: &[OutlineRule] = &[
    rule(Namespace, "(namespace_declaration name: (_) @name) @item"),
    rule(
        Namespace,
        "(file_scoped_namespace_declaration name: (_) @name) @item",
    ),
    rule(Class, "(class_declaration name: (identifier) @name) @item"),
    rule(Class, "(record_declaration name: (identifier) @name) @item"),
    rule(
        Struct,
        "(struct_declaration name: (identifier) @name) @item",
    ),
    rule(
        Interface,
        "(interface_declaration name: (identifier) @name) @item",
    ),
    rule(Enum, "(enum_declaration name: (identifier) @name) @item"),
    rule(
        Variant,
        "(enum_member_declaration name: (identifier) @name) @item",
    ),
    rule(
        Method,
        "(method_declaration name: (identifier) @name) @item",
    ),
    rule(
        Method,
        "(constructor_declaration name: (identifier) @name) @item",
    ),
    rule(
        Field,
        "(property_declaration name: (identifier) @name) @item",
    ),
];

pub(crate) const KOTLIN: &[OutlineRule] = &[
    rule(Class, "(class_declaration (type_identifier) @name) @item"),
    rule(Class, "(object_declaration (type_identifier) @name) @item"),
    rule(
        Function,
        "(function_declaration (simple_identifier) @name) @item",
    ),
    rule(
        Method,
        "(class_body (function_declaration (simple_identifier) @name) @item)",
    ),
    rule(Type, "(type_alias (type_identifier) @name) @item"),
];

pub(crate) const ZIG: &[OutlineRule] = &[
    rule(
        Function,
        "(function_declaration name: (identifier) @name) @item",
    ),
    rule(
        Constant,
        "(source_file (variable_declaration (identifier) @name) @item)",
    ),
];

pub(crate) const DART: &[OutlineRule] = &[
    rule(Class, "(class_declaration name: (identifier) @name) @item"),
    rule(Class, "(mixin_declaration name: (identifier) @name) @item"),
    rule(Enum, "(enum_declaration name: (identifier) @name) @item"),
    rule(
        Impl,
        "(extension_declaration name: (identifier) @name) @item",
    ),
    rule(
        Function,
        "(function_signature name: (identifier) @name) @item",
    ),
    rule(
        Method,
        "(method_signature (function_signature name: (identifier) @name)) @item",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar;
    use crate::language::{language_by_id, languages, plain_text};

    /// `(depth, kind, name, line)` for every symbol.
    fn outline_of(id: &str, text: &str) -> Vec<(usize, SymbolKind, String, usize)> {
        let language = language_by_id(id).unwrap_or_else(|| panic!("no language {id}"));
        outline(language, text)
            .into_iter()
            .map(|s| (s.depth, s.kind, s.name, s.line))
            .collect()
    }

    fn entry(
        depth: usize,
        kind: SymbolKind,
        name: &str,
        line: usize,
    ) -> (usize, SymbolKind, String, usize) {
        (depth, kind, name.to_owned(), line)
    }

    #[test]
    fn every_outline_compiles() {
        for spec in grammar::ALL {
            if !spec.outline.is_empty() {
                assert!(
                    spec.outline_query().is_some(),
                    "the {} outline does not compile",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn rust_outline() {
        let text = "\
const LIMIT: usize = 10;
static mut COUNT: u32 = 0;

pub struct Point {
    x: f32,
    y: f32,
}

enum Shape {
    Circle(f32),
    Square { side: f32 },
}

trait Area {
    type Unit;
    fn area(&self) -> f32;
    fn name(&self) -> String {
        String::new()
    }
}

impl Point {
    pub fn new() -> Self {
        fn helper() {}
        Point { x: 0.0, y: 0.0 }
    }
}

impl<T: Clone> Area for Wrapper<T> {
    fn area(&self) -> f32 { 0.0 }
}

mod inner {
    pub fn free() {}
}

type Alias = Vec<u8>;

macro_rules! twice { ($e:expr) => { $e; $e } }

fn main() {}
";
        assert_eq!(
            outline_of("rust", text),
            [
                entry(0, Constant, "LIMIT", 0),
                entry(0, Variable, "COUNT", 1),
                entry(0, Struct, "Point", 3),
                entry(1, Field, "x", 4),
                entry(1, Field, "y", 5),
                entry(0, Enum, "Shape", 8),
                entry(1, Variant, "Circle", 9),
                entry(1, Variant, "Square", 10),
                entry(2, Field, "side", 10),
                entry(0, Trait, "Area", 13),
                entry(1, Type, "Unit", 14),
                entry(1, Method, "area", 15),
                entry(1, Method, "name", 16),
                entry(0, Impl, "Point", 21),
                entry(1, Method, "new", 22),
                entry(2, Function, "helper", 23),
                entry(0, Impl, "Area for Wrapper<T>", 28),
                entry(1, Method, "area", 29),
                entry(0, Module, "inner", 32),
                entry(1, Function, "free", 33),
                entry(0, Type, "Alias", 36),
                entry(0, Macro, "twice", 38),
                entry(0, Function, "main", 40),
            ]
        );
    }

    #[test]
    fn python_outline() {
        let text = "\
VERSION = '1.0'

def top(a, b):
    def nested():
        pass
    return a

class Shape:
    sides = 0

    def area(self):
        return 0

    @staticmethod
    def unit():
        return Shape()

async def fetch():
    pass
";
        assert_eq!(
            outline_of("python", text),
            [
                entry(0, Variable, "VERSION", 0),
                entry(0, Function, "top", 2),
                entry(1, Function, "nested", 3),
                entry(0, Class, "Shape", 7),
                entry(1, Method, "area", 10),
                entry(1, Method, "unit", 14),
                entry(0, Function, "fetch", 17),
            ]
        );
    }

    #[test]
    fn javascript_outline() {
        let text = "\
const limit = 10;
export const double = (x) => x * 2;
function plain() {}
function* numbers() {}
class Shape {
  sides = 0;
  constructor() {}
  area() { return 0; }
  static unit() {}
}
var handler = function () {};
";
        assert_eq!(
            outline_of("javascript", text),
            [
                entry(0, Variable, "limit", 0),
                entry(0, Function, "double", 1),
                entry(0, Function, "plain", 2),
                entry(0, Function, "numbers", 3),
                entry(0, Class, "Shape", 4),
                entry(1, Field, "sides", 5),
                entry(1, Method, "constructor", 6),
                entry(1, Method, "area", 7),
                entry(1, Method, "unit", 8),
                entry(0, Function, "handler", 10),
            ]
        );
    }

    #[test]
    fn typescript_and_tsx_outline() {
        let text = "\
export interface Shape {
  sides: number;
  area(): number;
}
type Id = string | number;
enum Color { Red, Green }
abstract class Base {
  protected name: string = '';
  abstract describe(): string;
  greet(): void {}
}
namespace Geometry {
  export function unit(): Shape { return null as any; }
}
export const useThing = () => 1;
declare function external(x: number): void;
";
        let expected = [
            entry(0, Interface, "Shape", 0),
            entry(1, Field, "sides", 1),
            entry(1, Method, "area", 2),
            entry(0, Type, "Id", 4),
            entry(0, Enum, "Color", 5),
            entry(0, Class, "Base", 6),
            entry(1, Field, "name", 7),
            entry(1, Method, "describe", 8),
            entry(1, Method, "greet", 9),
            entry(0, Module, "Geometry", 11),
            entry(1, Function, "unit", 12),
            entry(0, Function, "useThing", 14),
            entry(0, Function, "external", 15),
        ];
        assert_eq!(outline_of("typescript", text), expected);
        assert_eq!(outline_of("tsx", text), expected);
        let component = "export function App() {\n  return <div className=\"app\">hi</div>;\n}\n";
        assert_eq!(outline_of("tsx", component), [entry(0, Function, "App", 0)]);
    }

    #[test]
    fn go_outline() {
        let text = "\
package main

const Limit = 10

var counter int

type Point struct {
	X, Y int
}

type Shape interface {
	Area() float64
}

type ID = string

type Celsius float64

func (p Point) Area() float64 { return 0 }

func main() {}
";
        assert_eq!(
            outline_of("go", text),
            [
                entry(0, Constant, "Limit", 2),
                entry(0, Variable, "counter", 4),
                entry(0, Struct, "Point", 6),
                entry(1, Field, "X", 7),
                entry(1, Field, "Y", 7),
                entry(0, Interface, "Shape", 10),
                entry(1, Method, "Area", 11),
                entry(0, Type, "ID", 14),
                entry(0, Type, "Celsius", 16),
                entry(0, Method, "Area", 18),
                entry(0, Function, "main", 20),
            ]
        );
    }

    #[test]
    fn c_outline() {
        let text = "\
#define LIMIT 10
#define SQUARE(x) ((x) * (x))

typedef unsigned long size;

struct point {
    int x;
    int y;
};

enum color { RED, GREEN };

static int counter = 0;

int add(int a, int b);

int add(int a, int b) {
    return a + b;
}

char *name(void) {
    return 0;
}
";
        assert_eq!(
            outline_of("c", text),
            [
                entry(0, Macro, "LIMIT", 0),
                entry(0, Macro, "SQUARE", 1),
                entry(0, Type, "size", 3),
                entry(0, Struct, "point", 5),
                entry(1, Field, "x", 6),
                entry(1, Field, "y", 7),
                entry(0, Enum, "color", 10),
                entry(1, Variant, "RED", 10),
                entry(1, Variant, "GREEN", 10),
                entry(0, Variable, "counter", 12),
                entry(0, Function, "add", 14),
                entry(0, Function, "add", 16),
                entry(0, Function, "name", 20),
            ]
        );
    }

    #[test]
    fn cpp_outline() {
        let text = "\
namespace geo {

class Shape {
public:
    Shape();
    virtual ~Shape();
    virtual double area() const = 0;
    int sides;
};

struct Point { int x; };

double Shape::perimeter() const { return 0; }

template <typename T>
T twice(T value) { return value + value; }

using Id = int;

}  // namespace geo

int main() { return 0; }
";
        assert_eq!(
            outline_of("cpp", text),
            [
                entry(0, Namespace, "geo", 0),
                entry(1, Class, "Shape", 2),
                entry(2, Method, "Shape", 4),
                entry(2, Method, "~Shape", 5),
                entry(2, Method, "area", 6),
                entry(2, Field, "sides", 7),
                entry(1, Struct, "Point", 10),
                entry(2, Field, "x", 10),
                entry(1, Method, "Shape::perimeter", 12),
                entry(1, Function, "twice", 15),
                entry(1, Type, "Id", 17),
                entry(0, Function, "main", 21),
            ]
        );
    }

    #[test]
    fn swift_outline() {
        let text = "\
let limit = 10

protocol Shape {
    var sides: Int { get }
    func area() -> Double
}

struct Point {
    var x: Double
    init() { x = 0 }
    func length() -> Double { return x }
}

class Circle: Shape {
    let sides = 0
    func area() -> Double { return 0 }
    deinit {}
}

enum Color {
    case red
    case green
    func name() -> String { return \"\" }
}

extension Point {
    func doubled() -> Point { return self }
}

typealias Id = Int

func main() {}
";
        assert_eq!(
            outline_of("swift", text),
            [
                entry(0, Variable, "limit", 0),
                entry(0, Interface, "Shape", 2),
                entry(1, Field, "sides", 3),
                entry(1, Method, "area", 4),
                entry(0, Struct, "Point", 7),
                entry(1, Field, "x", 8),
                entry(1, Method, "init", 9),
                entry(1, Method, "length", 10),
                entry(0, Class, "Circle", 13),
                entry(1, Field, "sides", 14),
                entry(1, Method, "area", 15),
                entry(1, Method, "deinit", 16),
                entry(0, Enum, "Color", 19),
                entry(1, Variant, "red", 20),
                entry(1, Variant, "green", 21),
                entry(1, Method, "name", 22),
                entry(0, Impl, "Point", 25),
                entry(1, Method, "doubled", 26),
                entry(0, Type, "Id", 29),
                entry(0, Function, "main", 31),
            ]
        );
    }

    #[test]
    fn markdown_outline_nests_by_heading_level() {
        let text = "\
# Title

Intro.

## Install `now`

### macOS

## Usage

Setext heading
==============

Another one
-----------

#

```sh
# not a heading
```
";
        assert_eq!(
            outline_of("markdown", text),
            [
                entry(0, Heading, "Title", 0),
                entry(1, Heading, "Install `now`", 4),
                entry(2, Heading, "macOS", 6),
                entry(1, Heading, "Usage", 8),
                entry(0, Heading, "Setext heading", 10),
                entry(1, Heading, "Another one", 13),
            ]
        );
    }

    #[test]
    fn other_languages_have_outlines_too() {
        assert_eq!(
            outline_of(
                "shell",
                "greet() {\n  echo hi\n}\nfunction bye {\n  echo bye\n}\n"
            ),
            [entry(0, Function, "greet", 0), entry(0, Function, "bye", 3)]
        );
        assert_eq!(
            outline_of("java", "class A {\n  int n;\n  A() {}\n  void run() {}\n  enum E { X }\n}\ninterface I { void go(); }\n"),
            [
                entry(0, Class, "A", 0),
                entry(1, Field, "n", 1),
                entry(1, Method, "A", 2),
                entry(1, Method, "run", 3),
                entry(1, Enum, "E", 4),
                entry(2, Variant, "X", 4),
                entry(0, Interface, "I", 6),
                entry(1, Method, "go", 6),
            ]
        );
        assert_eq!(
            outline_of(
                "ruby",
                "module M\n  class C\n    def run; end\n    def self.build; end\n  end\nend\n"
            ),
            [
                entry(0, Module, "M", 0),
                entry(1, Class, "C", 1),
                entry(2, Method, "run", 2),
                entry(2, Method, "build", 3),
            ]
        );
        assert_eq!(
            outline_of(
                "lua",
                "function a() end\nlocal function b() end\nfunction M.c() end\n"
            ),
            [
                entry(0, Function, "a", 0),
                entry(0, Function, "b", 1),
                entry(0, Function, "M.c", 2)
            ]
        );
        assert_eq!(
            outline_of("php", "<?php\nnamespace App;\ninterface I {}\ntrait T {}\nclass C {\n  function run() {}\n}\nfunction free() {}\n"),
            [
                entry(0, Namespace, "App", 1),
                entry(0, Interface, "I", 2),
                entry(0, Trait, "T", 3),
                entry(0, Class, "C", 4),
                entry(1, Method, "run", 5),
                entry(0, Function, "free", 7),
            ]
        );
        assert_eq!(
            outline_of("csharp", "namespace N {\n  class C {\n    int P { get; }\n    C() {}\n    void Run() {}\n  }\n  struct S {}\n  enum E { A }\n}\n"),
            [
                entry(0, Namespace, "N", 0),
                entry(1, Class, "C", 1),
                entry(2, Field, "P", 2),
                entry(2, Method, "C", 3),
                entry(2, Method, "Run", 4),
                entry(1, Struct, "S", 6),
                entry(1, Enum, "E", 7),
                entry(2, Variant, "A", 7),
            ]
        );
        assert_eq!(
            outline_of(
                "kotlin",
                "class C {\n  fun run() {}\n}\nobject O\nfun free() {}\ntypealias Id = Int\n"
            ),
            [
                entry(0, Class, "C", 0),
                entry(1, Method, "run", 1),
                entry(0, Class, "O", 3),
                entry(0, Function, "free", 4),
                entry(0, Type, "Id", 5),
            ]
        );
        assert_eq!(
            outline_of(
                "zig",
                "const std = @import(\"std\");\npub fn main() void {}\n"
            ),
            [entry(0, Constant, "std", 0), entry(0, Function, "main", 1)]
        );
        assert_eq!(
            outline_of(
                "dart",
                "class C {\n  void run() {}\n}\nenum E { a }\nvoid main() {}\n"
            ),
            [
                entry(0, Class, "C", 0),
                entry(1, Method, "run", 1),
                entry(0, Enum, "E", 3),
                entry(0, Function, "main", 4),
            ]
        );
    }

    #[test]
    fn languages_without_an_outline_return_nothing() {
        assert_eq!(outline(plain_text(), "# not markdown\nfn nope() {}\n"), []);
        for id in ["json", "toml", "yaml", "css", "html", "diff", "terraform"] {
            assert_eq!(outline_of(id, "a: 1\n"), [], "{id}");
        }
        for language in languages() {
            assert_eq!(outline(language, ""), [], "{}", language.id);
        }
    }

    #[test]
    fn broken_source_still_gives_the_rest_of_the_outline() {
        let text = "fn good() {}\n\nfn broken( {{{ ]]\n\nstruct After;\n\nfn also_good() {}\n";
        let names: Vec<String> = outline_of("rust", text)
            .into_iter()
            .map(|symbol| symbol.2)
            .collect();
        assert!(names.contains(&"good".to_owned()), "{names:?}");
        assert!(names.contains(&"also_good".to_owned()), "{names:?}");
        // Garbage of every kind must not panic.
        for language in languages() {
            outline(language, "}}}} ((( \u{0} \u{feff} é日本 ''' \"\"\" <<< ");
        }
    }

    #[test]
    fn names_and_positions_survive_multibyte_text_and_odd_line_ends() {
        let text = "// café ☕\r\nfn naïve() {}\u{c}fn second(\n    a: u8,\n) {}\n";
        let symbols = outline(language_by_id("rust").unwrap(), text);
        assert_eq!(symbols.len(), 2);
        assert_eq!((symbols[0].name.as_str(), symbols[0].line), ("naïve", 1));
        // The form feed is a line break for a rope, so `second` is on the third line.
        assert_eq!((symbols[1].name.as_str(), symbols[1].line), ("second", 2));
        for symbol in &symbols {
            assert!(text[symbol.byte..].starts_with(&symbol.name));
        }
    }

    #[test]
    fn long_names_are_collapsed_onto_one_line() {
        let text = "impl<T>\n    Display\n    for\n    Wrapper<\n        T,\n    >\n{\n}\n";
        assert_eq!(
            outline_of("rust", text),
            [entry(0, Impl, "Display for Wrapper< T, >", 3)]
        );
    }
}
