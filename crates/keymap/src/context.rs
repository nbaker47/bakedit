//! Key contexts and the predicates a keymap section uses to choose them.
//!
//! A port of gpui's `keymap/context.rs`. Every focused element contributes a [`KeyContext`];
//! the app hands the keymap the stack of them from the root (the workspace) down to the
//! focused element, and a section's `"context"` is a [`ContextPredicate`] over that stack.

use std::fmt;

/// What one element says about itself: identifiers (`Editor`, `menu`) and `key=value` pairs
/// (`mode=full`). A name appears at most once, as one or the other.
///
/// ```
/// use bakedit_keymap::KeyContext;
/// let editor = KeyContext::new("Editor").set("mode", "full").add("renaming");
/// assert!(editor.contains("renaming"));
/// assert_eq!(editor.get("mode"), Some("full"));
/// assert_eq!(editor.to_string(), "Editor mode=full renaming");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct KeyContext {
    entries: Vec<ContextEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ContextEntry {
    key: String,
    value: Option<String>,
}

impl KeyContext {
    /// A context with one identifier, usually the element's name. `KeyContext::default()` is
    /// the empty context.
    pub fn new(identifier: impl Into<String>) -> KeyContext {
        KeyContext::default().add(identifier)
    }

    /// Add an identifier. Nothing changes when the name is already present, as an identifier
    /// or as a key.
    // `add` is the name gpui gives this, and nothing here is being summed.
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, identifier: impl Into<String>) -> KeyContext {
        let key = identifier.into();
        if !self.contains(&key) {
            self.entries.push(ContextEntry { key, value: None });
        }
        self
    }

    /// Set `key` to `value`, replacing whatever the name stood for before (gpui keeps the
    /// first value instead; no Zed code relies on that).
    pub fn set(mut self, key: impl Into<String>, value: impl Into<String>) -> KeyContext {
        let key = key.into();
        let value = Some(value.into());
        match self.entries.iter_mut().find(|entry| entry.key == key) {
            Some(entry) => entry.value = value,
            None => self.entries.push(ContextEntry { key, value }),
        }
        self
    }

    /// Parse gpui's compact form: identifiers and `key=value` pairs separated by whitespace,
    /// with optional spaces around the `=` (`"Editor mode=full"`, `"baz foo = bar"`). Names
    /// and values are made of letters, digits, `_` and `-`. A name that appears twice keeps
    /// its first meaning, as in gpui. Anything else is an error; the empty string is the
    /// empty context.
    pub fn parse(source: &str) -> Result<KeyContext, ContextError> {
        let mut context = KeyContext::default();
        let mut rest = source.trim_start();
        while !rest.is_empty() {
            let (key, after) = split_identifier(rest);
            if key.is_empty() {
                return Err(ContextError::at(source, rest, unexpected(rest)));
            }
            rest = after.trim_start();
            match rest.strip_prefix('=') {
                Some(after) => {
                    let (value, after) = split_identifier(after.trim_start());
                    if !context.contains(key) {
                        context = context.set(key, value);
                    }
                    rest = after.trim_start();
                }
                None => context = context.add(key),
            }
        }
        Ok(context)
    }

    /// Whether `name` is present, as an identifier or as a key with any value.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|entry| entry.key == name)
    }

    /// The value of `key`. `None` when the key is absent and also when the name is only an
    /// identifier.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.key == key)?
            .value
            .as_deref()
    }

    /// Whether the context says nothing at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// gpui's compact form, `Editor mode=full renaming`. [`KeyContext::parse`] reads it back as
/// long as the names and values are the kind it accepts: non-empty, made of letters, digits,
/// `_` and `-`.
impl fmt::Display for KeyContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            match &entry.value {
                Some(value) => write!(f, "{}={}", entry.key, value)?,
                None => f.write_str(&entry.key)?,
            }
        }
        Ok(())
    }
}

/// The value of the `os` key on this machine, which Zed sets on its contexts so a keymap
/// can say `os == macos`.
const OS: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "windows") {
    "windows"
} else if cfg!(any(target_os = "linux", target_os = "freebsd")) {
    "linux"
} else {
    "unknown"
};

/// The `"context"` of a keymap section: a small boolean language over the context stack.
///
/// Grammar, from the loosest binding operator to the tightest: `A > B` (B is a descendant of
/// A), `A || B`, `A && B`, `key == value` and `key != value`, `!A`, with parentheses. So
/// `A && B > C` is `(A && B) > C` and `!A && B` is `(!A) && B`. All binary operators group
/// to the left.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContextPredicate {
    /// The context has this name, as an identifier or as a key with any value.
    Identifier(String),
    /// The context has this key with exactly this value.
    Equal(String, String),
    /// The context does not have this key with this value. True when the key is absent.
    NotEqual(String, String),
    /// The second predicate holds somewhere below a context where the first holds.
    Descendant(Box<ContextPredicate>, Box<ContextPredicate>),
    /// The predicate holds nowhere in the whole stack.
    Not(Box<ContextPredicate>),
    /// Both hold for the same context.
    And(Box<ContextPredicate>, Box<ContextPredicate>),
    /// Either holds.
    Or(Box<ContextPredicate>, Box<ContextPredicate>),
}

/// Why a context predicate (or a context) could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message} at column {column}")]
pub struct ContextError {
    /// What is wrong.
    pub message: String,
    /// Where: the 1-based character column in the text that was parsed.
    pub column: usize,
}

impl ContextError {
    /// An error at the point where `rest`, a suffix of `source`, begins.
    fn at(source: &str, rest: &str, message: String) -> ContextError {
        let consumed = source.len().saturating_sub(rest.len());
        let column = source
            .char_indices()
            .take_while(|(index, _)| *index < consumed)
            .count()
            + 1;
        ContextError { message, column }
    }
}

fn unexpected(rest: &str) -> String {
    match rest.chars().next() {
        Some(c) => format!("unexpected character '{c}'"),
        None => "unexpected end of the expression".to_string(),
    }
}

const PRECEDENCE_CHILD: u32 = 1;
const PRECEDENCE_OR: u32 = 2;
const PRECEDENCE_AND: u32 = 3;
const PRECEDENCE_EQ: u32 = 4;
const PRECEDENCE_NOT: u32 = 5;

/// The binary operators in the order gpui tries them.
const OPERATORS: [(&str, u32); 5] = [
    (">", PRECEDENCE_CHILD),
    ("&&", PRECEDENCE_AND),
    ("||", PRECEDENCE_OR),
    ("==", PRECEDENCE_EQ),
    ("!=", PRECEDENCE_EQ),
];

/// How deeply parentheses and `!` may nest. Parsing recurses once per level, so the limit
/// keeps a hostile keymap from overflowing the stack.
const MAX_DEPTH: usize = 64;

/// How many terms and operators one predicate may have. Evaluating and dropping a predicate
/// recurse along its longest chain, so this is a stack bound as well.
const MAX_NODES: usize = 1024;

impl ContextPredicate {
    /// Parse a section's `"context"` string.
    ///
    /// Names are made of letters, digits, `_` and `-`. As in gpui a name may also continue
    /// with (or consist of one of) the vim operator characters `>`, `<`, `~`, `"` and `?`,
    /// which is what lets Zed's vim keymap say `vim_operator == g~` and `vim_operator == >`.
    /// The price is gpui's: `Pane>Editor` without spaces is one odd name, not a descendant
    /// test. Both sides of `==` and `!=` must be plain names.
    ///
    /// Errors: an empty or unfinished expression, an unexpected character, a missing `)`,
    /// `==` or `!=` on something that is not a name, more than 64 levels of nesting or more
    /// than 1024 terms.
    pub fn parse(source: &str) -> Result<ContextPredicate, ContextError> {
        let mut parser = Parser {
            source,
            rest: source,
            depth: 0,
            nodes: 0,
        };
        parser.skip_whitespace();
        let predicate = parser.expression(0)?;
        if parser.rest.is_empty() {
            Ok(predicate)
        } else {
            Err(parser.error(unexpected(parser.rest)))
        }
    }

    /// Whether the predicate holds for the focused element: the last context of `contexts`,
    /// with the rest of the stack above it. Never true for an empty stack.
    pub fn eval(&self, contexts: &[KeyContext]) -> bool {
        self.eval_inner(contexts, contexts)
    }

    /// The deepest level at which the predicate holds: the largest `depth` for which it is
    /// true of `contexts[..depth]`, or `None` when it holds at no level. The result is never
    /// `Some(0)`, because nothing holds for an empty stack.
    pub fn depth_of(&self, contexts: &[KeyContext]) -> Option<usize> {
        (0..=contexts.len())
            .rev()
            .find(|depth| self.eval_inner(&contexts[..*depth], contexts))
    }

    /// Evaluate against `contexts`, whose last element is the current context, with
    /// `all_contexts` being the stack that `!` searches.
    ///
    /// `key == value` and `key != value` read the current context only. The key `os` is
    /// `macos`, `linux` or `windows` in every context that does not set it, because Zed sets
    /// it on its own contexts and keymaps test it.
    fn eval_inner(&self, contexts: &[KeyContext], all_contexts: &[KeyContext]) -> bool {
        let Some(context) = contexts.last() else {
            return false;
        };
        match self {
            ContextPredicate::Identifier(name) => context.contains(name) || name == "os",
            ContextPredicate::Equal(key, value) => value_of(context, key) == Some(value.as_str()),
            ContextPredicate::NotEqual(key, value) => {
                value_of(context, key) != Some(value.as_str())
            }
            ContextPredicate::Not(predicate) => !(0..all_contexts.len())
                .any(|last| predicate.eval_inner(&all_contexts[..=last], all_contexts)),
            ContextPredicate::Descendant(parent, child) => {
                // The first level where the parent holds decides: the child is then judged
                // against what lies below it, and sees nothing above.
                for last in 0..contexts.len() - 1 {
                    if parent.eval_inner(&contexts[..=last], all_contexts) {
                        let below = &contexts[last + 1..];
                        return child.eval_inner(below, below);
                    }
                }
                false
            }
            ContextPredicate::And(left, right) => {
                left.eval_inner(contexts, all_contexts) && right.eval_inner(contexts, all_contexts)
            }
            ContextPredicate::Or(left, right) => {
                left.eval_inner(contexts, all_contexts) || right.eval_inner(contexts, all_contexts)
            }
        }
    }

    /// Whether this predicate certainly holds wherever `other` holds. A conservative test, as
    /// in gpui: it is true for an equal predicate, for one of this predicate's `||` branches,
    /// and when `other` is an `&&` with a term this covers or a `>` whose child this covers.
    /// It decides whether a `null` binding in one section hides a binding in another when no
    /// context stack is at hand.
    pub fn is_superset(&self, other: &ContextPredicate) -> bool {
        if self == other {
            return true;
        }
        if let ContextPredicate::Or(left, right) = self {
            return left.is_superset(other) || right.is_superset(other);
        }
        match other {
            ContextPredicate::Descendant(_, child) => self.is_superset(child),
            ContextPredicate::And(left, right) => self.is_superset(left) || self.is_superset(right),
            _ => false,
        }
    }

    /// Write an `&&` or `||` chain flat, with parentheses only around the other operator.
    fn fmt_chain(&self, f: &mut fmt::Formatter<'_>, and: bool, first: &mut bool) -> fmt::Result {
        match (and, self) {
            (true, ContextPredicate::And(left, right))
            | (false, ContextPredicate::Or(left, right)) => {
                left.fmt_chain(f, and, first)?;
                right.fmt_chain(f, and, first)
            }
            (_, term) => {
                if !*first {
                    f.write_str(if and { " && " } else { " || " })?;
                }
                *first = false;
                match term {
                    ContextPredicate::And(..) | ContextPredicate::Or(..) => write!(f, "({term})"),
                    _ => write!(f, "{term}"),
                }
            }
        }
    }
}

/// The predicate in keymap syntax, as gpui prints it: `a && (b || c) && d`, `!(a > b)`.
/// It is meant for showing a binding's context. Parsing it back gives an equivalent
/// predicate for the forms keymaps use, but `>` chains and `>` under `&&` or `||` are written
/// without the parentheses they would need.
impl fmt::Display for ContextPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContextPredicate::Identifier(name) => f.write_str(name),
            ContextPredicate::Equal(key, value) => write!(f, "{key} == {value}"),
            ContextPredicate::NotEqual(key, value) => write!(f, "{key} != {value}"),
            ContextPredicate::Descendant(parent, child) => write!(f, "{parent} > {child}"),
            ContextPredicate::Not(predicate) => match predicate.as_ref() {
                ContextPredicate::Identifier(name) => write!(f, "!{name}"),
                other => write!(f, "!({other})"),
            },
            ContextPredicate::And(..) => self.fmt_chain(f, true, &mut true),
            ContextPredicate::Or(..) => self.fmt_chain(f, false, &mut true),
        }
    }
}

/// The value a predicate sees for `key` in `context`, including the implicit `os`.
fn value_of<'a>(context: &'a KeyContext, key: &str) -> Option<&'a str> {
    match context.get(key) {
        Some(value) => Some(value),
        None if key == "os" && !context.contains(key) => Some(OS),
        None => None,
    }
}

struct Parser<'a> {
    source: &'a str,
    rest: &'a str,
    depth: usize,
    nodes: usize,
}

impl Parser<'_> {
    fn error(&self, message: String) -> ContextError {
        ContextError::at(self.source, self.rest, message)
    }

    fn skip_whitespace(&mut self) {
        self.rest = self.rest.trim_start();
    }

    /// Step over `len` bytes of operator or name and the whitespace after them.
    fn advance(&mut self, len: usize) {
        self.rest = &self.rest[len..];
        self.skip_whitespace();
    }

    /// Count one more node of the tree being built.
    fn node(&mut self, predicate: ContextPredicate) -> Result<ContextPredicate, ContextError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(self.error(format!("more than {MAX_NODES} terms")));
        }
        Ok(predicate)
    }

    /// A primary followed by every operator that binds at least as tightly as
    /// `min_precedence`. This is gpui's precedence climbing, operator order included.
    fn expression(&mut self, min_precedence: u32) -> Result<ContextPredicate, ContextError> {
        let mut predicate = self.primary()?;
        'operators: loop {
            for (operator, precedence) in OPERATORS {
                if self.rest.starts_with(operator) && precedence >= min_precedence {
                    self.advance(operator.len());
                    let right = self.expression(precedence + 1)?;
                    predicate = self.combine(operator, predicate, right)?;
                    continue 'operators;
                }
            }
            return Ok(predicate);
        }
    }

    fn combine(
        &mut self,
        operator: &str,
        left: ContextPredicate,
        right: ContextPredicate,
    ) -> Result<ContextPredicate, ContextError> {
        let (left, right) = match operator {
            "==" | "!=" => match (left, right) {
                (ContextPredicate::Identifier(key), ContextPredicate::Identifier(value)) => {
                    let equality = if operator == "==" {
                        ContextPredicate::Equal(key, value)
                    } else {
                        ContextPredicate::NotEqual(key, value)
                    };
                    // The two names became one node.
                    self.nodes = self.nodes.saturating_sub(1);
                    return Ok(equality);
                }
                _ => return Err(self.error(format!("operands of {operator} must be names"))),
            },
            _ => (Box::new(left), Box::new(right)),
        };
        self.node(match operator {
            ">" => ContextPredicate::Descendant(left, right),
            "&&" => ContextPredicate::And(left, right),
            _ => ContextPredicate::Or(left, right),
        })
    }

    fn primary(&mut self) -> Result<ContextPredicate, ContextError> {
        let Some(next) = self.rest.chars().next() else {
            return Err(self.error(unexpected(self.rest)));
        };
        match next {
            '(' => {
                self.descend()?;
                self.advance(1);
                let predicate = self.expression(0)?;
                if !self.rest.starts_with(')') {
                    return Err(self.error("expected a ')'".to_string()));
                }
                self.advance(1);
                self.depth -= 1;
                Ok(predicate)
            }
            '!' => {
                self.descend()?;
                self.advance(1);
                let predicate = self.expression(PRECEDENCE_NOT)?;
                self.depth -= 1;
                self.node(ContextPredicate::Not(Box::new(predicate)))
            }
            c if is_identifier_char(c) => {
                let len = self
                    .rest
                    .find(|c: char| !is_identifier_char(c) && !is_vim_operator_char(c))
                    .unwrap_or(self.rest.len());
                self.name(len)
            }
            c if is_vim_operator_char(c) => self.name(c.len_utf8()),
            _ => Err(self.error(unexpected(self.rest))),
        }
    }

    /// Take the first `len` bytes as a name.
    fn name(&mut self, len: usize) -> Result<ContextPredicate, ContextError> {
        let name = self.rest[..len].to_string();
        self.advance(len);
        self.node(ContextPredicate::Identifier(name))
    }

    fn descend(&mut self) -> Result<(), ContextError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error(format!("nested more than {MAX_DEPTH} levels deep")));
        }
        Ok(())
    }
}

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn is_vim_operator_char(c: char) -> bool {
    matches!(c, '>' | '<' | '~' | '"' | '?')
}

/// Split `text` after its leading run of identifier characters.
fn split_identifier(text: &str) -> (&str, &str) {
    let len = text
        .find(|c: char| !is_identifier_char(c))
        .unwrap_or(text.len());
    text.split_at(len)
}

#[cfg(test)]
mod tests {
    use super::ContextPredicate::*;
    use super::*;

    fn parse(source: &str) -> ContextPredicate {
        ContextPredicate::parse(source).unwrap_or_else(|e| panic!("{source:?}: {e}"))
    }

    fn id(name: &str) -> Box<ContextPredicate> {
        Box::new(Identifier(name.to_string()))
    }

    fn eq(key: &str, value: &str) -> Box<ContextPredicate> {
        Box::new(Equal(key.to_string(), value.to_string()))
    }

    fn ne(key: &str, value: &str) -> Box<ContextPredicate> {
        Box::new(NotEqual(key.to_string(), value.to_string()))
    }

    fn and(a: Box<ContextPredicate>, b: Box<ContextPredicate>) -> Box<ContextPredicate> {
        Box::new(And(a, b))
    }

    fn or(a: Box<ContextPredicate>, b: Box<ContextPredicate>) -> Box<ContextPredicate> {
        Box::new(Or(a, b))
    }

    fn not(a: Box<ContextPredicate>) -> Box<ContextPredicate> {
        Box::new(Not(a))
    }

    fn child(a: Box<ContextPredicate>, b: Box<ContextPredicate>) -> Box<ContextPredicate> {
        Box::new(Descendant(a, b))
    }

    /// A context stack from gpui's compact form, one context per `/` separated part.
    fn stack(text: &str) -> Vec<KeyContext> {
        text.split('/')
            .map(|part| KeyContext::parse(part).unwrap_or_else(|e| panic!("{part:?}: {e}")))
            .collect()
    }

    #[test]
    fn builds_contexts() {
        let context = KeyContext::new("Editor")
            .set("mode", "full")
            .add("vim_mode");
        assert!(context.contains("Editor"));
        assert!(context.contains("mode"));
        assert!(context.contains("vim_mode"));
        assert!(!context.contains("full"));
        assert!(!context.contains("Pane"));
        assert_eq!(context.get("mode"), Some("full"));
        assert_eq!(context.get("Editor"), None);
        assert_eq!(context.get("missing"), None);
        assert!(!context.is_empty());
        assert!(KeyContext::default().is_empty());
        assert_eq!(context.to_string(), "Editor mode=full vim_mode");
        assert_eq!(KeyContext::default().to_string(), "");
    }

    #[test]
    fn a_name_appears_once() {
        // Adding twice changes nothing; setting replaces.
        let context = KeyContext::new("Editor").add("Editor").set("mode", "full");
        assert_eq!(context, KeyContext::new("Editor").set("mode", "full"));
        let context = context.set("mode", "auto_height");
        assert_eq!(context.get("mode"), Some("auto_height"));
        assert_eq!(context.to_string(), "Editor mode=auto_height");
        // A key is not demoted to an identifier, an identifier can be given a value.
        let context = context.add("mode").set("Editor", "yes");
        assert_eq!(context.to_string(), "Editor=yes mode=auto_height");
    }

    #[test]
    fn parses_contexts() {
        let expected = KeyContext::new("baz").set("foo", "bar");
        assert_eq!(KeyContext::parse("baz foo=bar").unwrap(), expected);
        assert_eq!(KeyContext::parse("baz foo = bar").unwrap(), expected);
        assert_eq!(
            KeyContext::parse("  baz foo   =   bar baz").unwrap(),
            expected
        );
        assert_eq!(KeyContext::parse(" baz foo = bar").unwrap(), expected);
        // The first meaning of a name is kept, as in gpui.
        assert_eq!(
            KeyContext::parse("a=1 a=2 a").unwrap(),
            KeyContext::default().set("a", "1")
        );
        assert_eq!(KeyContext::parse("").unwrap(), KeyContext::default());
        assert_eq!(KeyContext::parse("   ").unwrap(), KeyContext::default());
        assert_eq!(
            KeyContext::parse("vim_mode=helix-normal").unwrap(),
            KeyContext::default().set("vim_mode", "helix-normal")
        );
        for context in ["Editor mode=full renaming", "Workspace", "a=1 b c=3"] {
            assert_eq!(KeyContext::parse(context).unwrap().to_string(), context);
        }
        // gpui recurses for ever on these; here they are errors.
        let error = KeyContext::parse("Editor !mode").unwrap_err();
        assert_eq!(error.to_string(), "unexpected character '!' at column 8");
        assert!(KeyContext::parse("(a)").is_err());
        assert!(KeyContext::parse("= b").is_err());
    }

    #[test]
    fn parses_identifiers() {
        assert_eq!(parse("abc12"), *id("abc12"));
        assert_eq!(parse("_1a"), *id("_1a"));
        assert_eq!(parse("  Editor  "), *id("Editor"));
        assert_eq!(parse("not-editing"), *id("not-editing"));
        assert_eq!(parse("éditeur"), *id("éditeur"));
    }

    #[test]
    fn parses_negations() {
        assert_eq!(parse("!abc"), *not(id("abc")));
        assert_eq!(parse(" ! ! abc"), *not(not(id("abc"))));
        // `!` binds tighter than everything else.
        assert_eq!(parse("!a && b"), *and(not(id("a")), id("b")));
        assert_eq!(parse("!a || b"), *or(not(id("a")), id("b")));
        assert_eq!(parse("!a > b"), *child(not(id("a")), id("b")));
        assert_eq!(parse("!(a && b)"), *not(and(id("a"), id("b"))));
        assert_eq!(parse("!(a == b)"), *not(eq("a", "b")));
    }

    #[test]
    fn parses_equality_operators() {
        assert_eq!(parse("a == b"), *eq("a", "b"));
        assert_eq!(parse("c!=d"), *ne("c", "d"));
        assert_eq!(parse("mode==full"), *eq("mode", "full"));
        let error = ContextPredicate::parse("c == !d").unwrap_err();
        assert_eq!(error.message, "operands of == must be names");
        let error = ContextPredicate::parse("(a && b) != c").unwrap_err();
        assert_eq!(error.message, "operands of != must be names");
        assert!(ContextPredicate::parse("!a == b").is_err());
        assert!(ContextPredicate::parse("a == b == c").is_err());
    }

    #[test]
    fn parses_boolean_operators() {
        assert_eq!(parse("a || b"), *or(id("a"), id("b")));
        assert_eq!(
            parse("a || !b && c"),
            *or(id("a"), and(not(id("b")), id("c")))
        );
        assert_eq!(
            parse("a && b || c&&d"),
            *or(and(id("a"), id("b")), and(id("c"), id("d")))
        );
        assert_eq!(
            parse("a == b && c || d == e && f"),
            *or(and(eq("a", "b"), id("c")), and(eq("d", "e"), id("f")))
        );
        assert_eq!(
            parse("a && b && c && d"),
            *and(and(and(id("a"), id("b")), id("c")), id("d"))
        );
        assert_eq!(parse("a || b || c"), *or(or(id("a"), id("b")), id("c")));
    }

    #[test]
    fn parses_parenthesized_expressions() {
        assert_eq!(
            parse("a && (b == c || d != e)"),
            *and(id("a"), or(eq("b", "c"), ne("d", "e")))
        );
        assert_eq!(parse(" ( a || b ) "), *or(id("a"), id("b")));
        assert_eq!(parse("((a))"), *id("a"));
        assert_eq!(parse("(a || b) && c"), *and(or(id("a"), id("b")), id("c")));
    }

    #[test]
    fn descendant_binds_loosest() {
        assert_eq!(parse("a > b"), *child(id("a"), id("b")));
        assert_eq!(parse("A && B > C"), *child(and(id("A"), id("B")), id("C")));
        assert_eq!(parse("A > B && C"), *child(id("A"), and(id("B"), id("C"))));
        assert_eq!(
            parse("A || B > C || D"),
            *child(or(id("A"), id("B")), or(id("C"), id("D")))
        );
        assert_eq!(parse("A > B > C"), *child(child(id("A"), id("B")), id("C")));
        assert_eq!(
            parse("A > (B > C)"),
            *child(id("A"), child(id("B"), id("C")))
        );
        assert_eq!(parse("A > B == c"), *child(id("A"), eq("B", "c")));
    }

    #[test]
    fn parses_the_contexts_of_real_keymaps() {
        assert_eq!(
            parse("Editor && mode == full"),
            *and(id("Editor"), eq("mode", "full"))
        );
        assert_eq!(parse("Editor || Pane"), *or(id("Editor"), id("Pane")));
        assert_eq!(
            parse("ProjectPanel && not_editing"),
            *and(id("ProjectPanel"), id("not_editing"))
        );
        assert_eq!(
            parse("!AcpThread > Editor && mode == full"),
            *child(not(id("AcpThread")), and(id("Editor"), eq("mode", "full")))
        );
        assert_eq!(
            parse("(CollabPanel && editing) > Editor"),
            *child(and(id("CollabPanel"), id("editing")), id("Editor"))
        );
        assert_eq!(
            parse("BufferSearchBar && !in_replace > Editor"),
            *child(
                and(id("BufferSearchBar"), not(id("in_replace"))),
                id("Editor")
            )
        );
        assert_eq!(
            parse("FileFinder || (FileFinder > Picker > Editor)"),
            *or(
                id("FileFinder"),
                child(child(id("FileFinder"), id("Picker")), id("Editor"))
            )
        );
        assert_eq!(
            parse("VimControl && !menu || !Editor && !Terminal"),
            *or(
                and(id("VimControl"), not(id("menu"))),
                and(not(id("Editor")), not(id("Terminal")))
            )
        );
        assert_eq!(
            parse("vim_mode == insert && !(showing_code_actions || showing_completions)"),
            *and(
                eq("vim_mode", "insert"),
                not(or(id("showing_code_actions"), id("showing_completions")))
            )
        );
    }

    #[test]
    fn names_may_carry_vim_operator_characters() {
        assert_eq!(parse("vim_operator == g~"), *eq("vim_operator", "g~"));
        assert_eq!(parse("vim_operator == g?"), *eq("vim_operator", "g?"));
        assert_eq!(parse("vim_operator == >"), *eq("vim_operator", ">"));
        assert_eq!(parse("vim_operator == <"), *eq("vim_operator", "<"));
        assert_eq!(parse("vim_operator == \""), *eq("vim_operator", "\""));
        assert_eq!(parse("vim_operator == gU"), *eq("vim_operator", "gU"));
        assert_eq!(
            parse("vim_operator == > || vim_operator == <"),
            *or(eq("vim_operator", ">"), eq("vim_operator", "<"))
        );
        // gpui's quirk: without spaces this is one name, not a descendant test.
        assert_eq!(parse("Pane>Editor"), *id("Pane>Editor"));
        assert_eq!(parse("Pane > Editor"), *child(id("Pane"), id("Editor")));
        // A lone operator character is a one-character name and stops there.
        assert_eq!(parse("a > >"), *child(id("a"), id(">")));
    }

    #[test]
    fn reports_parse_errors_with_their_column() {
        for (source, message, column) in [
            ("", "unexpected end of the expression", 1),
            ("   ", "unexpected end of the expression", 4),
            ("a &&", "unexpected end of the expression", 5),
            ("a && ", "unexpected end of the expression", 6),
            ("a b", "unexpected character 'b'", 3),
            ("a & b", "unexpected character '&'", 3),
            ("a | b", "unexpected character '|'", 3),
            ("a = b", "unexpected character '='", 3),
            ("&& a", "unexpected character '&'", 1),
            ("(a", "expected a ')'", 3),
            ("(a && b", "expected a ')'", 8),
            ("a)", "unexpected character ')'", 2),
            ("a !b", "unexpected character '!'", 3),
            ("a >= b", "unexpected character '='", 4),
            ("é && $", "unexpected character '$'", 6),
            ("c == !d", "operands of == must be names", 8),
        ] {
            let error = ContextPredicate::parse(source).expect_err(source);
            assert_eq!(
                (error.message.as_str(), error.column),
                (message, column),
                "{source:?}"
            );
        }
        let error = ContextPredicate::parse("Editor &&").unwrap_err();
        assert_eq!(
            error.to_string(),
            "unexpected end of the expression at column 10"
        );
    }

    #[test]
    fn refuses_predicates_that_could_exhaust_the_stack() {
        let deep = format!("{}a{}", "(".repeat(64), ")".repeat(64));
        assert_eq!(parse(&deep), *id("a"));
        let too_deep = format!("{}a{}", "(".repeat(65), ")".repeat(65));
        let error = ContextPredicate::parse(&too_deep).unwrap_err();
        assert!(error.message.contains("nested"), "{error}");
        let error = ContextPredicate::parse(&format!("{}a", "!".repeat(100_000))).unwrap_err();
        assert!(error.message.contains("nested"), "{error}");
        let error = ContextPredicate::parse(&"(".repeat(100_000)).unwrap_err();
        assert!(error.message.contains("nested"), "{error}");

        let long = vec!["a"; 512].join(" && ");
        assert!(ContextPredicate::parse(&long).is_ok());
        let too_long = vec!["a"; 100_000].join(" && ");
        let error = ContextPredicate::parse(&too_long).unwrap_err();
        assert!(error.message.contains("terms"), "{error}");
        let equalities = vec!["a == b"; 400].join(" || ");
        assert!(ContextPredicate::parse(&equalities).is_ok());
    }

    #[test]
    fn evaluates_against_the_current_context() {
        let cases: &[(&str, &str, bool)] = &[
            ("Editor", "Editor", true),
            ("Editor", "Pane", false),
            ("Editor", "Workspace/Pane/Editor", true),
            // Only the last context is the current one.
            ("Pane", "Workspace/Pane/Editor", false),
            ("Workspace", "Workspace/Pane/Editor", false),
            // A key counts as a name too.
            ("mode", "Editor mode=full", true),
            ("full", "Editor mode=full", false),
            ("mode == full", "Editor mode=full", true),
            ("mode == full", "Editor mode=auto_height", false),
            ("mode == full", "Editor", false),
            ("mode == full", "Editor mode", false),
            ("mode != full", "Editor mode=full", false),
            ("mode != full", "Editor mode=auto_height", true),
            ("mode != full", "Editor", true),
            ("mode != full", "Editor mode", true),
            ("Editor && mode == full", "Editor mode=full", true),
            ("Editor && mode == full", "Editor", false),
            ("Editor && mode == full", "Pane mode=full", false),
            ("Editor || Pane", "Pane", true),
            ("Editor || Pane", "Editor", true),
            ("Editor || Pane", "Terminal", false),
            ("Editor || Pane", "Workspace/Pane/Editor mode=full", true),
            (
                "ProjectPanel && not_editing",
                "Workspace/Dock/ProjectPanel not_editing",
                true,
            ),
            (
                "ProjectPanel && not_editing",
                "Workspace/Dock/ProjectPanel editing",
                false,
            ),
            ("a && (b || c)", "a c", true),
            ("a && (b || c)", "a", false),
            ("a && b || c", "c", true),
        ];
        for (predicate, contexts, expected) in cases {
            assert_eq!(
                parse(predicate).eval(&stack(contexts)),
                *expected,
                "{predicate:?} in {contexts:?}"
            );
        }
        // Nothing holds for an empty stack, not even a negation.
        for predicate in ["Editor", "!Editor", "mode != full", "a || !a"] {
            assert!(!parse(predicate).eval(&[]), "{predicate}");
        }
    }

    #[test]
    fn child_operator() {
        let predicate = parse("parent > child");
        assert!(predicate.eval(&stack("parent/child")));
        assert!(predicate.eval(&stack("grandparent/parent/child")));
        assert!(!predicate.eval(&stack("other/child")));
        assert!(predicate.eval(&stack("parent/other/child")));
        assert!(!predicate.eval(&[]));
        assert!(!predicate.eval(&stack("child")));
        assert!(!predicate.eval(&stack("parent")));
        assert!(!predicate.eval(&stack("parent/child/other")));

        let zany = parse("child > child");
        assert!(!zany.eval(&stack("child")));
        assert!(zany.eval(&stack("child/child")));

        let predicate = parse("a > b > c");
        assert!(predicate.eval(&stack("a/b/c")));
        assert!(predicate.eval(&stack("a/x/b/y/c")));
        assert!(!predicate.eval(&stack("a/c")));
        assert!(!predicate.eval(&stack("b/a/c")));

        // The first level where the parent holds decides, even where a later one would do:
        // below the first `a` there is a `b`, below the second there is none.
        let predicate = parse("a > !b");
        assert!(!predicate.eval(&stack("a/b/a/c")));
        assert!(predicate.eval(&stack("x/b/a/c")));
        assert!(predicate.eval(&stack("a/c")));
    }

    #[test]
    fn not_operator() {
        let not_editor = parse("!editor");
        assert!(not_editor.eval(&stack("workspace")));
        assert!(!not_editor.eval(&stack("editor")));
        // `!` looks at the whole stack, not just the current context.
        assert!(!not_editor.eval(&stack("editor/workspace")));
        assert!(!not_editor.eval(&stack("workspace/editor")));

        let complex_not = parse("!editor && workspace");
        assert!(complex_not.eval(&stack("workspace")));
        assert!(!complex_not.eval(&stack("editor/workspace")));

        let not_mode = parse("!(mode == full)");
        assert!(!not_mode.eval(&stack("mode=full")));
        assert!(not_mode.eval(&stack("mode=partial")));

        let not_descendant = parse("!(parent > child)");
        assert!(not_descendant.eval(&stack("parent")));
        assert!(not_descendant.eval(&stack("child")));
        assert!(!not_descendant.eval(&stack("parent/child")));

        let descendant_not = parse("parent > !child");
        assert!(!descendant_not.eval(&stack("parent")));
        assert!(!descendant_not.eval(&stack("child")));
        assert!(!descendant_not.eval(&stack("parent/child")));

        let double_not = parse("!!editor");
        assert!(double_not.eval(&stack("editor")));
        assert!(!double_not.eval(&stack("workspace")));

        let workspace_pane_editor = stack("Workspace/Pane/Editor");
        assert!(!parse("Pane > (Pane > Editor)").eval(&workspace_pane_editor));
        assert!(parse("Workspace > Pane > Editor").eval(&workspace_pane_editor));
        assert!(!parse("(Pane > Pane) > Editor").eval(&workspace_pane_editor));

        // Below a `>` the negation only sees what lies below the parent.
        let pane_not_workspace = parse("Pane > !Workspace");
        assert!(pane_not_workspace.eval(&stack("Pane/Editor")));
        assert!(!pane_not_workspace.eval(&stack("Pane/Workspace")));
        assert!(pane_not_workspace.eval(&workspace_pane_editor));

        let not_workspace = parse("!Workspace");
        assert!(!not_workspace.eval(&stack("Workspace")));
        assert!(not_workspace.eval(&stack("Pane")));
        assert!(not_workspace.eval(&stack("Editor")));
        assert!(!not_workspace.eval(&workspace_pane_editor));

        // The agent editor is excluded, an ordinary editor is not.
        let predicate = parse("!AcpThread > Editor && mode == full");
        assert!(predicate.eval(&stack("Workspace/Pane/Editor mode=full")));
        assert!(!predicate.eval(&stack("Workspace/AcpThread/Editor mode=full")));
        assert!(!predicate.eval(&stack("Editor mode=full")));
    }

    #[test]
    fn depth_is_the_deepest_level_that_matches() {
        let contexts = stack("Workspace/Pane/Editor mode=full");
        let cases: &[(&str, Option<usize>)] = &[
            ("Workspace", Some(1)),
            ("Pane", Some(2)),
            ("Editor", Some(3)),
            ("Editor && mode == full", Some(3)),
            ("Editor || Pane", Some(3)),
            ("Pane || Workspace", Some(2)),
            ("Workspace > Pane", Some(2)),
            ("Workspace > Editor", Some(3)),
            ("Pane > Editor", Some(3)),
            ("Terminal", None),
            ("Editor > Pane", None),
            ("mode == auto_height", None),
            // Absence holds at every level, so it counts as the deepest.
            ("mode != auto_height", Some(3)),
            ("mode != full", Some(2)),
            ("!Terminal", Some(3)),
            ("!Editor", None),
            ("Workspace && !Terminal", Some(1)),
        ];
        for (predicate, depth) in cases {
            assert_eq!(parse(predicate).depth_of(&contexts), *depth, "{predicate}");
        }
        assert_eq!(parse("Editor").depth_of(&[]), None);
        assert_eq!(parse("!Editor").depth_of(&[]), None);
        // The same name twice: the deepest occurrence.
        assert_eq!(parse("Pane").depth_of(&stack("Pane/Pane/Editor")), Some(2));
    }

    #[test]
    fn os_is_implied_in_every_context() {
        let this_os = format!("os == {OS}");
        assert!(parse(&this_os).eval(&stack("Editor")));
        assert!(parse(&format!("Editor && {this_os}")).eval(&stack("Workspace/Editor")));
        assert!(!parse(&format!("os != {OS}")).eval(&stack("Editor")));
        assert!(!parse("os == plan9").eval(&stack("Editor")));
        assert!(parse("os != plan9").eval(&stack("Editor")));
        assert!(parse("os").eval(&stack("Editor")));
        assert_eq!(parse(&this_os).depth_of(&stack("Workspace/Pane")), Some(2));
        // Still nothing for an empty stack, and a context can say otherwise.
        assert!(!parse(&this_os).eval(&[]));
        assert!(parse("os == plan9").eval(&stack("Editor os=plan9")));
        assert!(!parse(&this_os).eval(&stack("Editor os=plan9")));
        assert!(["macos", "linux", "windows", "unknown"].contains(&OS));
    }

    #[test]
    fn superset() {
        for (a, b, expected) in [
            ("editor", "editor", true),
            ("editor", "workspace", false),
            ("editor", "editor && vim_mode", true),
            ("editor", "mode == full && editor", true),
            ("editor && mode == full", "editor", false),
            ("editor", "something > editor", true),
            ("editor", "editor > menu", false),
            ("foo || bar || baz", "bar", true),
            ("foo || bar || baz", "quux", false),
            ("Editor || Pane", "Pane && active", true),
            ("mode == full", "Editor && mode == full", true),
            ("!menu", "Editor && !menu", true),
            ("!menu", "!Editor", false),
        ] {
            assert_eq!(
                parse(a).is_superset(&parse(b)),
                expected,
                "{a:?} over {b:?}"
            );
        }
    }

    #[test]
    fn displays_in_keymap_syntax() {
        let cases = [
            (id("a"), "a"),
            (eq("a", "b"), "a == b"),
            (ne("a", "b"), "a != b"),
            (child(id("a"), id("b")), "a > b"),
            (not(id("a")), "!a"),
            (not(and(id("a"), id("b"))), "!(a && b)"),
            (not(or(id("a"), id("b"))), "!(a || b)"),
            (and(id("a"), id("b")), "a && b"),
            (and(and(id("a"), id("b")), id("c")), "a && b && c"),
            (or(id("a"), id("b")), "a || b"),
            (or(or(id("a"), id("b")), id("c")), "a || b || c"),
            (or(id("a"), and(id("b"), id("c"))), "a || (b && c)"),
            (
                and(
                    and(and(id("a"), eq("b", "c")), not(child(id("d"), id("e")))),
                    eq("f", "g"),
                ),
                "a && b == c && !(d > e) && f == g",
            ),
            (
                and(and(id("a"), or(id("b"), id("c"))), id("d")),
                "a && (b || c) && d",
            ),
            (
                or(or(id("a"), and(id("b"), id("c"))), id("d")),
                "a || (b && c) || d",
            ),
        ];
        for (predicate, expected) in cases {
            assert_eq!(predicate.to_string(), expected);
            assert_eq!(parse(expected), *predicate);
        }
        assert_eq!(
            parse("Editor&&mode==full").to_string(),
            "Editor && mode == full"
        );
    }
}
