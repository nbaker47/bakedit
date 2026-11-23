//! Grammars: a tree-sitter language and the queries that drive highlighting, loaded on
//! first use.
//!
//! Every grammar crate bundles a highlights query and some bundle injections and locals
//! queries. They are used as shipped, with three per-grammar decisions made here because the
//! queries were written for different highlighters:
//!
//! * [`Precedence`]: when two patterns capture the same node, which one wins.
//! * [`InjectedChildren`]: what an injection leaves out of the injected document.
//! * Extra patterns (the `*_EXTRA` constants) laid over a bundled query where it has an
//!   outright bug or draws the language unlike Zed does. For the languages Zed itself ships
//!   (Rust, Python, JavaScript, TypeScript, TSX, JSON, YAML, Markdown, shell, Go, C, C++,
//!   CSS, diff) the extras bring the result to within a fraction of a percent of Zed's own
//!   query, measured by the style the built-in theme gives each byte of a few megabytes of
//!   real code. The other languages are as their grammars' queries draw them.

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
//
// Taking one out of a grammar's `highlights` list returns that language to its bundled
// query. They were written against each grammar's node types to reproduce what Zed's
// queries (which are GPL-licensed) make of the same text; short as they are, several are
// bound to resemble the patterns they agree with.
// ---------------------------------------------------------------------------------------

/// tree-sitter-rust, brought into line with the query Zed uses for Rust:
///
/// * Numbers and booleans are filed under `constant.builtin`; Zed has `number` and `boolean`.
/// * The all-caps pattern never matches because of a stray quote in its regex
///   (`"^[A-Z][A-Z\\d_]+$'"`), so constants come out as constructors. As in Zed, the pattern
///   here also takes a single capital (`N`) and leading underscores (`_UNUSED`).
/// * A capitalised name is a type even where it is called: `Ok(value)` and `Some(value)` are
///   the colour of a type, not of a function. So is a variant in an enum's definition,
///   whatever its capitalisation. These come after the macro patterns and before the one
///   for parameters, as in Zed, so `DEFINE_GUID!` is a constant and a parameter a parameter.
/// * A macro invoked through a path (`log::info!`), the name of a `macro_rules!` definition
///   and the fragment specifiers in one are not captured at all.
/// * Only `*`, `&` and `'` are captured as operators. The list here is the one Zed uses. It
///   includes `<`, `>` and `|`, so the patterns that make them brackets around generics and
///   a closure's parameters come after it. They say which of the pair each one is, which is
///   what lets [`bracket_marks`](crate::bracket_marks) colour them by depth as Zed does.
/// * The apostrophe of a lifetime is captured as an operator and its name as a label; Zed
///   calls both `lifetime`.
/// * A whole attribute is captured as one `attribute`. In Zed only its name and its plain
///   top-level arguments are: the `#`, the path before a `::`, and arguments nested inside
///   another (`test` in `#[cfg(not(test))]`) are left plain.
const RUST_EXTRA: &str = r##"
[(integer_literal) (float_literal)] @number
(boolean_literal) @boolean

(macro_invocation macro: (scoped_identifier name: (identifier) @function.macro))
(macro_invocation "!" @function.macro)
(macro_definition name: (identifier) @function.macro)
(fragment_specifier) @type

((identifier) @type
 (#match? @type "^[A-Z]"))
((identifier) @constant
 (#match? @constant "^_*[A-Z][A-Z\\d_]*$"))
(enum_variant name: (identifier) @type)
(parameter (identifier) @variable.parameter)
(shorthand_field_identifier) @property

[
  "!=" "%" "%=" "&" "&=" "&&" "*" "*=" "+" "+=" "-" "-=" "->" ".." "..=" "..." "/="
  ":" "<<" "<<=" "<" "<=" "=" "==" "=>" ">" ">=" ">>" ">>=" "@" "^" "^=" "|" "|=" "||" "?"
] @operator
(unary_expression "!" @operator)
(binary_expression operator: "/" @operator)

(type_arguments "<" @punctuation.bracket.open ">" @punctuation.bracket.close)
(type_parameters "<" @punctuation.bracket.open ">" @punctuation.bracket.close)
(bracketed_type "<" @punctuation.bracket.open ">" @punctuation.bracket.close)
(for_lifetimes "<" @punctuation.bracket.open ">" @punctuation.bracket.close)
(closure_parameters "|" @punctuation.bracket.open "|" @punctuation.bracket.close)

"'" @lifetime
(lifetime (identifier) @lifetime)

(attribute_item "#" @punctuation.special)
(inner_attribute_item "#" @punctuation.special)
(inner_attribute_item "!" @none)
(attribute (scoped_identifier path: (_) @none))
(attribute (token_tree (token_tree) @none))
((attribute (token_tree (identifier) @none . "::"))
 (#match? @none "^[a-z\\d_]*$"))
"##;

/// `true` and `false` are filed under `constant.builtin` by the bundled queries of the
/// languages that use this; Zed colours them as booleans.
const TRUE_FALSE_EXTRA: &str = "[(true) (false)] @boolean\n";

/// As [`TRUE_FALSE_EXTRA`], for C#, whose grammar has one node for both literals.
const BOOLEAN_LITERAL_EXTRA: &str = "(boolean_literal) @boolean\n";

/// As [`TRUE_FALSE_EXTRA`], for PHP.
const PHP_EXTRA: &str = "(boolean) @boolean\n";

/// tree-sitter-python, brought into line with the query Zed uses for Python. The bundled
/// query lists its method-call pattern before the one that makes every attribute a property,
/// so `obj.method()` comes out as a property; and beyond that bug the two differ in most
/// lines of real code:
///
/// * Names follow convention wherever they stand, attributes included: `CamelCase` is a
///   class, `ALL_CAPS` a constant, either with leading underscores.
/// * The name of a class where it is defined and its base classes are types, as are the
///   names in an annotation and the second argument of `isinstance`.
/// * Calling a class (`Path(...)`) is the colour of a type; `int`, `str`, `list` and the
///   other built-in types are built-in types, called or in an annotation.
/// * A keyword argument's name is `function.kwargs`.
/// * `and`, `or`, `not`, `in` and `is` are keyword operators, not operators.
/// * Of a decorator only the `@` and the names are function-coloured, not its punctuation;
///   `@property`, `@classmethod` and `@staticmethod` are built-in attributes.
/// * The well-known dunder attributes (`__name__`, `__dict__`) are special attributes.
const PYTHON_EXTRA: &str = r#"
((identifier) @type.class
 (#match? @type.class "^_*[A-Z][A-Za-z0-9_]*$"))
((identifier) @constant
 (#match? @constant "^_*[A-Z][A-Z0-9_]*$"))
(type (identifier) @type)
(generic_type (identifier) @type)
(type (tuple (identifier) @type))
(type (string) @type)
(type_alias_statement "type" @keyword)

(call function: (attribute attribute: (identifier) @function.method))
(call function: (identifier) @function)
(decorator
  "@" @function.decorator
  [
    (identifier) @function.decorator
    (attribute
      object: (identifier) @function.decorator
      attribute: (identifier) @function.decorator)
    (call function: (identifier) @function.decorator)
    (call
      (attribute
        object: (identifier) @function.decorator
        attribute: (identifier) @function.decorator))
  ])
(function_definition name: (identifier) @function)
((call
   function: (identifier) @_function
   arguments: (argument_list (_) (identifier) @type))
 (#eq? @_function "isinstance"))
((call
   function: (identifier) @_function
   arguments: (argument_list (identifier) @type (identifier) @type))
 (#eq? @_function "issubclass"))
(function_definition
  parameters: (parameters
    [
      (identifier) @variable.parameter
      (typed_parameter (identifier) @variable.parameter)
      (default_parameter name: (identifier) @variable.parameter)
      (typed_default_parameter name: (identifier) @variable.parameter)
      (list_splat_pattern (identifier) @variable.parameter)
      (dictionary_splat_pattern (identifier) @variable.parameter)
      (typed_parameter (list_splat_pattern (identifier) @variable.parameter))
      (typed_parameter (dictionary_splat_pattern (identifier) @variable.parameter))
    ]))
(call arguments: (argument_list (keyword_argument name: (identifier) @function.kwargs)))

(class_definition name: (identifier) @type.class)
(class_definition superclasses: (argument_list (identifier) @type.class))
((call function: (identifier) @type.class)
 (#match? @type.class "^_*[A-Z][A-Za-z0-9_]*$"))
((call function: (identifier) @function.builtin)
 (#any-of? @function.builtin
   "abs" "aiter" "all" "anext" "any" "ascii" "bin" "bool" "breakpoint" "bytearray" "bytes"
   "callable" "chr" "classmethod" "compile" "complex" "delattr" "dict" "dir" "divmod"
   "enumerate" "eval" "exec" "filter" "float" "format" "frozenset" "getattr" "globals"
   "hasattr" "hash" "help" "hex" "id" "input" "int" "isinstance" "issubclass" "iter" "len"
   "list" "locals" "map" "max" "memoryview" "min" "next" "object" "oct" "open" "ord" "pow"
   "print" "property" "range" "repr" "reversed" "round" "set" "setattr" "slice" "sorted"
   "staticmethod" "str" "sum" "super" "tuple" "type" "vars" "zip" "__import__"))

[(true) (false)] @boolean
(ellipsis) @constant.builtin
["." "," ":"] @punctuation.delimiter
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
(interpolation "{" @punctuation.special "}" @punctuation.special)
(binary_operator operator: "@" @operator)
["and" "in" "is" "not" "or" "is not" "not in"] @keyword.operator

((decorator "@" @attribute.builtin (identifier) @attribute.builtin @_name)
 (#any-of? @_name "classmethod" "staticmethod" "property"))
((attribute attribute: (identifier) @attribute.special)
 (#match? @attribute.special "^__(all|annotations|bases|builtins|class|closure|code|debug|defaults|dict|doc|file|func|globals|kwdefaults|match_args|members|metaclass|methods|module|mro|mro_entries|name|qualname|post_init|self|signature|slots|subclasses|version|weakref|wrapped|classcell|spec|path|package|future|traceback)__$"))
([
   (call function: (identifier) @type.builtin)
   (type (identifier) @type.builtin)
   (generic_type (identifier) @type.builtin)
   (type (binary_operator left: (identifier) @type.builtin))
 ]
 (#any-of? @type.builtin
   "bool" "bytearray" "bytes" "complex" "dict" "float" "frozenset" "int" "list" "memoryview"
   "object" "range" "set" "slice" "str" "tuple"))
"#;

/// tree-sitter-javascript and tree-sitter-typescript, brought into line with the queries Zed
/// uses for JavaScript, TypeScript and TSX. This part is shared by all three:
///
/// * The bundled queries treat every capitalised identifier as a constructor or a type. In
///   Zed a capital alone means nothing: a name is a type because of where it stands (after
///   `new`, in an `extends` clause, in a type annotation, as a JSX tag). So `React` in an
///   import or a component passed as a value is a plain variable, while a function
///   definition with a capitalised name is still a function.
/// * `Promise`, `Object`, `Array` and the other global classes are types where a static
///   method is called on them.
/// * A constant is `ALL_CAPS` with any leading underscores.
/// * Shorthand and private properties are properties, `constructor` is a constructor.
/// * Escapes in strings, the delimiters and flags of a regex, `...` and the `?` and `:` of
///   a conditional are highlighted.
const JAVASCRIPT_EXTRA: &str = r#"
((identifier) @variable
 (#match? @variable "^[A-Z]"))
((call_expression function: (member_expression object: (identifier) @type))
 (#any-of? @type
   "Promise" "Array" "Object" "Map" "Set" "WeakMap" "WeakSet" "Date" "Error" "TypeError"
   "RangeError" "SyntaxError" "ReferenceError" "EvalError" "URIError" "RegExp" "Function"
   "Number" "String" "Boolean" "Symbol" "BigInt" "Proxy" "ArrayBuffer" "DataView"))
[
  (shorthand_property_identifier)
  (shorthand_property_identifier_pattern)
  (private_property_identifier)
] @property

(call_expression function: (identifier) @function)
(call_expression
  function: (member_expression property: (private_property_identifier) @function.method))
(new_expression constructor: (identifier) @type.class)
(function_declaration name: (identifier) @function)
(function_expression name: (identifier) @function)
(generator_function_declaration name: (identifier) @function)
(method_definition name: (private_property_identifier) @function.method)
((method_definition name: (property_identifier) @constructor)
 (#eq? @constructor "constructor"))
(variable_declarator
  name: (identifier) @function
  value: [(function_expression) (arrow_function)])
(assignment_expression
  left: (identifier) @function
  right: [(function_expression) (arrow_function)])
(catch_clause parameter: (identifier) @variable.parameter)
(arrow_function parameter: (identifier) @variable.parameter)

([
   (identifier)
   (shorthand_property_identifier)
   (shorthand_property_identifier_pattern)
 ] @constant
 (#match? @constant "^_*[A-Z_][A-Z\\d_]*$"))
[(true) (false)] @boolean
(escape_sequence) @string.escape
(regex "/" @string.regex)
(regex_flags) @keyword.operator.regex
"..." @operator
(ternary_expression ["?" ":"] @operator)
"#;

/// What only the JavaScript grammar needs: there a class's name and the class it extends are
/// plain identifiers, and a destructured parameter is a pattern in the parameter list.
const JAVASCRIPT_ONLY_EXTRA: &str = r#"
(class_declaration name: (identifier) @type.class)
(class name: (identifier) @type.class)
(class_heritage (identifier) @type.class)
(formal_parameters
  [
    (identifier) @variable.parameter
    (_ [(identifier) (shorthand_property_identifier_pattern)] @variable.parameter)
    (_ (object_assignment_pattern
         left: (shorthand_property_identifier_pattern) @variable.parameter))
    (assignment_pattern left: (identifier) @variable.parameter)
  ])
([
   (identifier)
   (shorthand_property_identifier)
   (shorthand_property_identifier_pattern)
 ] @constant
 (#match? @constant "^_*[A-Z_][A-Z\\d_]*$"))
"#;

/// What the TypeScript grammars need, for TypeScript and TSX:
///
/// * The class in an `extends` clause and the namespace of a qualified type are types.
/// * The names of parameters, destructured ones included, are parameters rather than the
///   properties their shorthand form would make them.
/// * The angle brackets of type parameters are brackets, as those of type arguments are;
///   the `|` and `&` of union and intersection types are punctuation, not operators.
/// * `infer`, `is` and `module` are keywords, and the `this` type is a built-in type.
const TYPESCRIPT_EXTRA: &str = r#"
(this_type) @type.builtin
(extends_clause value: (identifier) @type.class)
(nested_type_identifier module: (identifier) @type)
[
  (required_parameter (identifier) @variable.parameter)
  (required_parameter (_ [(identifier) (shorthand_property_identifier_pattern)] @variable.parameter))
  (required_parameter
    (_ (object_assignment_pattern
         left: (shorthand_property_identifier_pattern) @variable.parameter)))
  (required_parameter (_ (assignment_pattern left: (identifier) @variable.parameter)))
  (optional_parameter (identifier) @variable.parameter)
  (optional_parameter (_ [(identifier) (shorthand_property_identifier_pattern)] @variable.parameter))
  (optional_parameter
    (_ (object_assignment_pattern
         left: (shorthand_property_identifier_pattern) @variable.parameter)))
  (optional_parameter (_ (assignment_pattern left: (identifier) @variable.parameter)))
  (index_signature name: (identifier) @variable.parameter)
  (type_predicate name: (identifier) @variable.parameter)
]
([
   (identifier)
   (shorthand_property_identifier)
   (shorthand_property_identifier_pattern)
 ] @constant
 (#match? @constant "^_*[A-Z_][A-Z\\d_]*$"))
(template_literal_type) @string
(template_type "${" @punctuation.special "}" @punctuation.special)
(type_parameters "<" @punctuation.bracket ">" @punctuation.bracket)
(type_arguments "<" @punctuation.bracket ">" @punctuation.bracket)
(union_type "|" @punctuation.special)
(intersection_type "&" @punctuation.special)
["infer" "is" "module"] @keyword
"#;

/// TypeScript only, as in Zed: `null`, `undefined`, `true` and `false` used as types are
/// built-in types, and so are the names in a type-only import. The angle brackets of
/// generics are marked as pairs here and not for TSX, because Zed colours them by depth in
/// TypeScript and leaves every angle bracket alone where there are JSX tags.
const TYPESCRIPT_ONLY_EXTRA: &str = r#"
(type_arguments "<" @punctuation.bracket.open ">" @punctuation.bracket.close)
(type_parameters "<" @punctuation.bracket.open ">" @punctuation.bracket.close)
(literal_type [(null) (undefined) (true) (false)] @type.builtin)
(import_specifier "type" name: (identifier) @type)
(import_specifier "type" alias: (identifier) @type)
(import_statement "type"
  (import_clause (named_imports (import_specifier name: (identifier) @type))))
(import_statement "type"
  (import_clause (named_imports (import_specifier alias: (identifier) @type))))
"#;

/// JSX, for JavaScript and TSX: a component's tag is a tag like any other, not a type, down
/// to the parts of `<Menu.Item>`; and the `=` of an attribute is punctuation.
const JSX_EXTRA: &str = r#"
[
  (jsx_opening_element name: (identifier) @tag.component.jsx)
  (jsx_closing_element name: (identifier) @tag.component.jsx)
  (jsx_self_closing_element name: (identifier) @tag.component.jsx)
]
[
  (jsx_opening_element name: (member_expression
    object: (identifier) @tag.component.jsx property: (property_identifier) @tag.component.jsx))
  (jsx_closing_element name: (member_expression
    object: (identifier) @tag.component.jsx property: (property_identifier) @tag.component.jsx))
  (jsx_self_closing_element name: (member_expression
    object: (identifier) @tag.component.jsx property: (property_identifier) @tag.component.jsx))
]
[
  (jsx_opening_element name: (member_expression
    object: (member_expression
      object: (identifier) @tag.component.jsx property: (property_identifier) @tag.component.jsx)
    property: (property_identifier) @tag.component.jsx))
  (jsx_closing_element name: (member_expression
    object: (member_expression
      object: (identifier) @tag.component.jsx property: (property_identifier) @tag.component.jsx)
    property: (property_identifier) @tag.component.jsx))
  (jsx_self_closing_element name: (member_expression
    object: (member_expression
      object: (identifier) @tag.component.jsx property: (property_identifier) @tag.component.jsx)
    property: (property_identifier) @tag.component.jsx))
]
([
   (jsx_opening_element name: (identifier) @tag.jsx)
   (jsx_closing_element name: (identifier) @tag.jsx)
   (jsx_self_closing_element name: (identifier) @tag.jsx)
 ]
 (#match? @tag.jsx "^[a-z][^.]*$"))
(jsx_attribute (property_identifier) @attribute.jsx)
(jsx_attribute "=" @punctuation.delimiter.jsx)
(html_character_reference) @string.special
"#;

/// tree-sitter-toml captures the whole `key = value` pair as a property instead of the key,
/// which leaves a bare key coloured as a type while a dotted one is a property.
const TOML_EXTRA: &str = "(pair (bare_key) @property)\n";

/// tree-sitter-bash's query is a sketch: commands, variables, a few keywords and operators.
/// Zed's shell scripts look different in almost every line, so the rest is filled in here to
/// match:
///
/// * A bare word is a string, as a quoted one is; a word that starts with `-` stays a
///   constant and the name of a command a function.
/// * A variable's name is a plain variable, not a property.
/// * `local` and `declare` are keywords, a `#!` line is a directive, numbers are numbers,
///   test operators (`-n`, `-d`) are keyword operators and `case` patterns are regexes.
/// * The rest of the operators and the punctuation, including the `$`, `${` and `$(` of an
///   expansion.
/// * The `)` that ends a `case` pattern is not a bracket. Calling it a delimiter keeps bracket
///   matching from pairing it with an unrelated `(`.
const BASH_EXTRA: &str = r###"
[(word) (ansi_c_string) (heredoc_end)] @string
(variable_name) @variable
(special_variable_name) @variable.special
["local" "declare"] @keyword
(command_name (word) @function)
(command_name [(string) (raw_string) (ansi_c_string)] @string)
(function_definition name: (word) @function)
((command (_) @constant)
 (#match? @constant "^-"))
(number) @number
(regex) @string.regex
(case_item value: (_) @string.regex)
(test_operator) @keyword.operator
((program . (comment) @keyword.directive)
 (#match? @keyword.directive "^#![ \t]*/"))

[
  "||" "<<" ">&" ">&-" ":" "//" "/" "%" "%%" "#" "##" "=" "=~" "==" "!=" "+=" "-=" "*=" "/="
  "%=" "**=" "<<=" ">>=" "&=" "^=" "|=" "-o" "-a" "^" "&" "<=" ">=" "+" "-" "*" "**" "!" "++"
  "--" "~" "?"
] @operator

";" @punctuation.delimiter
["(" ")" "{" "}" "[" "]"] @punctuation.bracket
(test_command ["[[" "]]"] @punctuation.bracket)
(compound_statement ["((" "))"] @punctuation.bracket)
(simple_expansion "$" @punctuation.special)
(expansion "${" @punctuation.special "}" @punctuation.special)
(command_substitution "$(" @punctuation.special ")" @punctuation.special)
(arithmetic_expansion ["$((" "$[" "))" "]"] @punctuation.special)
(case_item ")" @punctuation.delimiter)
"###;

/// tree-sitter-kotlin's pattern for a function's name requires the name to be the
/// declaration's first child, so a function with a modifier (`override fun`, `private fun`,
/// `suspend fun`) is not recognised. A declaration has only one identifier of its own.
const KOTLIN_EXTRA: &str = "(function_declaration (simple_identifier) @function)\n";

/// tree-sitter-go, where Zed draws Go differently. These go first, because this grammar's
/// query is written specific-first:
///
/// * A name declared with `const` is a constant, and the key of a struct literal a property.
/// * A method in an interface is a method, a type parameter a type, and the predeclared
///   types are built-in types.
const GO_EXTRA: &str = r#"
(const_spec name: (identifier) @constant)
(keyed_element . (literal_element (identifier) @property))
(method_elem name: (field_identifier) @function.method)
(type_parameter_declaration name: (identifier) @type)
((type_identifier) @type.builtin
 (#any-of? @type.builtin
   "any" "bool" "byte" "comparable" "complex64" "complex128" "error" "float32" "float64" "int"
   "int8" "int16" "int32" "int64" "rune" "string" "uint" "uint8" "uint16" "uint32" "uint64"
   "uintptr"))
((call_expression function: (identifier) @function.builtin)
 (#any-of? @function.builtin "clear" "max" "min"))
"#;

/// tree-sitter-c, where Zed draws C differently (C++ takes these too, before its own
/// patterns):
///
/// * A character literal is a string, not a number, and escapes in strings and characters
///   are escapes.
/// * `/`, `%`, `?`, `:` and the rest of the operators are operators; `goto` is a keyword.
/// * A constant may start with underscores (`_WIN32`).
/// * A function pointer called through a field (`self->input.read(...)`) is a property.
/// * Attributes, GNU and C23, are attributes.
const C_EXTRA: &str = r##"
["constexpr" "goto" "#elifdef" "#elifndef"] @keyword
[
  "=" "+=" "-=" "*=" "/=" "%=" "&=" "|=" "^=" "<<=" ">>=" "++" "--" "+" "-" "*" "/" "%" "~" "&"
  "|" "^" "<<" ">>" "!" "&&" "||" "==" "!=" "<" ">" "<=" ">=" "->" "?" ":"
] @operator
"," @punctuation.delimiter
["{" "}" "(" ")" "[" "]"] @punctuation.bracket
(char_literal) @string
(escape_sequence) @string.escape
((identifier) @constant
 (#match? @constant "^_*[A-Z][A-Z\\d_]*$"))
(call_expression function: (identifier) @function)
(function_declarator declarator: (identifier) @function)
(preproc_function_def name: (identifier) @function.special)
(field_identifier) @property
(attribute_specifier) @attribute
(attribute_specifier (argument_list (identifier) @attribute))
(attribute prefix: (identifier) @attribute)
(attribute name: (identifier) @attribute)
"##;

/// tree-sitter-cpp, where Zed draws C++ differently. These come last, after C's patterns
/// and C++'s own:
///
/// * A method called through `.` or `->` is a function (in C it is a property), as are a
///   function reached through nested namespaces, an operator's name and a destructor's.
/// * A name in capitals is a built-in constant even where it is called, which is how
///   function-like macros stand out.
/// * `.`, `.*`, `->*`, `<=>` and the word operators are operators; `:` and `::` are
///   punctuation except in a conditional expression.
/// * More keywords, `static_assert`, and `defined` and its argument in a preprocessor
///   condition.
/// * The angle brackets of a template's parameters and arguments are a pair of brackets.
const CPP_EXTRA: &str = r#"
(call_expression (qualified_identifier (identifier) @function.call))
(call_expression (qualified_identifier (qualified_identifier (identifier) @function.call)))
(call_expression
  (qualified_identifier (qualified_identifier (qualified_identifier (identifier) @function.call))))
(call_expression function: (identifier) @function)
(call_expression function: (field_expression field: (field_identifier) @function))
(template_function name: (identifier) @function)
(template_method name: (field_identifier) @function)
(function_declarator declarator: (identifier) @function)
(function_declarator declarator: (qualified_identifier name: (identifier) @function))
(function_declarator declarator: (field_identifier) @function)
(operator_name) @function
(operator_name (identifier) @operator)
(destructor_name (identifier) @function)
((identifier) @constant.builtin
 (#match? @constant.builtin "^_*[A-Z][A-Z\\d_]*$"))
"static_assert" @function.builtin
[
  "alignas" "alignof" "decltype" "operator" "thread_local" "typedef" "struct" "union" "enum"
  "extern" "inline" "sizeof"
  (storage_class_specifier)
  (type_qualifier)
] @keyword
(preproc_defined "defined" @keyword.preproc)
(preproc_defined (identifier) @constant.macro)
[(null) "nullptr"] @constant.builtin
[":" "::"] @punctuation.delimiter
[
  "." ".*" "->*" "and" "and_eq" "bitand" "bitor" "compl" "not" "not_eq" "or" "or_eq" "xor"
  "xor_eq"
] @operator
"<=>" @operator.spaceship
(template_argument_list "<" @punctuation.bracket.open ">" @punctuation.bracket.close)
(template_parameter_list "<" @punctuation.bracket.open ">" @punctuation.bracket.close)
(conditional_expression ":" @operator)
(user_defined_literal (literal_suffix) @operator)
"#;

/// tree-sitter-yaml, where Zed draws YAML differently: an anchor and an alias are types (the
/// bundled query calls them labels, which the theme leaves plain), escapes in a quoted
/// string are escapes, and a key is a property wherever it stands, including the one-pair
/// mappings inside a flow sequence.
const YAML_EXTRA: &str = r#"
[(anchor_name) (alias_name)] @type
(escape_sequence) @string.escape
key: (flow_node
  [
    (plain_scalar (string_scalar))
    (double_quote_scalar)
    (single_quote_scalar)
  ] @property)
"#;

/// tree-sitter-css, where Zed draws a style sheet differently:
///
/// * Class, id and pseudo selectors are selectors (which the built-in theme leaves plain),
///   not properties and attributes.
/// * A plain value (`none`, `bold`), the name of a keyframe and a media keyword are built-in
///   constants; `and`, `or`, `not` and `only` are keyword operators.
/// * The `#` of a colour is part of the colour.
const CSS_EXTRA: &str = r##"
(id_name) @selector.id
(class_name) @selector.class
(pseudo_element_selector "::" (tag_name) @selector.pseudo)
(pseudo_class_selector ":" (class_name) @selector.pseudo)
[(plain_value) (keyframes_name) (keyword_query)] @constant.builtin
(attribute_selector (plain_value) @string)
(parenthesized_query (keyword_query) @property)
([(property_name) (plain_value)] @variable
 (#match? @variable "^--"))
["and" "or" "not" "only"] @keyword.operator
["[" "]"] @punctuation.bracket
"|" @operator
(color_value "#" @string.special)
"##;

/// tree-sitter-diff, where Zed draws a patch differently: the `diff` of a command line is a
/// function and its arguments parameters, a file mode and a similarity score are numbers,
/// and `index` is a keyword.
const DIFF_EXTRA: &str = r#"
(command "diff" @function (argument) @variable.parameter)
(mode) @number
(index "index" @keyword)
(similarity (score) @number "%" @number)
["+" "++" "+++" "++++" "-" "--" "---" "----" ".."] @punctuation.special
[(binary_change) (similarity) (file_change)] @label
"#;

/// tree-sitter-md's block query, where Zed draws a Markdown file differently:
///
/// * The `#` of a heading, the underline of a setext heading and a thematic break are part
///   of the title, not punctuation.
/// * The fence of a code block and the language named after it are punctuation, not part of
///   a string.
/// * An indented code block is plain text, as the body of a fenced one in an unknown language
///   is.
const MARKDOWN_EXTRA: &str = r#"
[
  (atx_h1_marker)
  (atx_h2_marker)
  (atx_h3_marker)
  (atx_h4_marker)
  (atx_h5_marker)
  (atx_h6_marker)
  (setext_h1_underline)
  (setext_h2_underline)
  (thematic_break)
] @title.markup
[(fenced_code_block_delimiter) (info_string)] @punctuation.embedded.markup
(indented_code_block) @none
"#;

/// tree-sitter-md's inline query, likewise:
///
/// * The asterisks of emphasis and the backticks of a code span are drawn with what they
///   enclose, and the parentheses of a link with its address.
/// * A backslash escape is not highlighted, and an e-mail address in angle brackets is a
///   link like a URL is.
const MARKDOWN_INLINE_EXTRA: &str = r#"
(emphasis (emphasis_delimiter) @text.emphasis)
(strong_emphasis (emphasis_delimiter) @text.strong)
(code_span (code_span_delimiter) @text.literal)
(inline_link ["(" ")"] @text.uri)
(image ["(" ")"] @text.uri)
(email_autolink) @text.uri
[(backslash_escape) (hard_line_break)] @none
"#;

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

/// See [`MARKDOWN_INLINE_HTML_INJECTION`] for the first pattern. The second is missing from
/// tree-sitter-md's query altogether: the cells of a table hold inline markup just as a
/// paragraph does, and the grammar's own Rust parser treats them so.
const MARKDOWN_BLOCK_INJECTIONS: &str = r#"
((html_block) @injection.content
 (#set! injection.language "html")
 (#set! injection.combined))

((pipe_table_cell) @injection.content
 (#set! injection.language "markdown_inline"))
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
        JAVASCRIPT_ONLY_EXTRA,
        JSX_EXTRA,
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
        TYPESCRIPT_EXTRA,
        TYPESCRIPT_ONLY_EXTRA,
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
        TYPESCRIPT_EXTRA,
        JSX_EXTRA,
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
    highlights: &[tree_sitter_yaml::HIGHLIGHTS_QUERY, YAML_EXTRA],
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
    highlights: &[tree_sitter_md::HIGHLIGHT_QUERY_BLOCK, MARKDOWN_EXTRA],
    injections: &[
        tree_sitter_md::INJECTION_QUERY_BLOCK,
        MARKDOWN_BLOCK_INJECTIONS,
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
    highlights: &[
        tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
        MARKDOWN_INLINE_EXTRA,
    ],
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
    highlights: &[TRUE_FALSE_EXTRA, GO_EXTRA, tree_sitter_go::HIGHLIGHTS_QUERY],
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
    highlights: &[tree_sitter_c::HIGHLIGHT_QUERY, TRUE_FALSE_EXTRA, C_EXTRA],
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
        C_EXTRA,
        tree_sitter_cpp::HIGHLIGHT_QUERY,
        TRUE_FALSE_EXTRA,
        CPP_EXTRA,
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
    highlights: &[tree_sitter_css::HIGHLIGHTS_QUERY, CSS_EXTRA],
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
    highlights: &[tree_sitter_diff::HIGHLIGHTS_QUERY, DIFF_EXTRA],
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
    /// * `label` (goto labels), `lifetime` (Rust's), `link_text` (the text of a
    ///   Markdown link, a branch name in a commit message) and `selector.*` (the class, id
    ///   and pseudo selectors of a style sheet) are in Zed's vocabulary but the theme does
    ///   not colour them.
    /// * `error` and `text.*` are tree-sitter-make's: a stray `/` in an automatic variable
    ///   and the message of `$(error ...)`, `$(warning ...)` and `$(info ...)`.
    const UNSTYLED: &[&str] = &[
        "embedded",
        "none",
        "label",
        "lifetime",
        "link_text",
        "selector.class",
        "selector.id",
        "selector.pseudo",
        "error",
        "text.danger",
        "text.warning",
        "text.note",
    ];

    /// Captures in the bundled queries that are not highlights at all.
    const NOT_HIGHLIGHTS: &[&str] = &[
        "_class",
        "_function",
        "_name",
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
