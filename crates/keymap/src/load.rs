//! Reading Zed's `keymap.json` format into a [`Keymap`].
//!
//! The format is an array of sections, each `{ "context": "...", "bindings": { "<keys>":
//! <action> } }`, written as JSON with comments and trailing commas. Loading follows Zed's
//! `KeymapFile::load_keymap`, except that nothing is fatal: whatever cannot be used is
//! skipped and reported.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use bakedit_core::{jsonc, Action};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::context::ContextPredicate;
use crate::keymap::{Binding, Keymap, KeymapSource, Target, NO_ACTION, SEQUENCE, UNBIND};
use crate::keystroke::Keystroke;

/// Something in a keymap file that was skipped, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapWarning {
    /// The 0-based index of the section in the file's array. `None` when the file as a whole
    /// could not be used.
    pub section: Option<usize>,
    /// The section's `"context"` as written. `None` when it has none (or none that is text).
    pub context: Option<String>,
    /// The keystrokes of the binding concerned, as written. `None` when the problem is with
    /// the section itself.
    pub key: Option<String>,
    /// What is wrong.
    pub message: String,
}

/// One line for a log or a notification, counting sections from one:
/// `section 3 (context "Editor"), binding "cmd-k (": unknown key ...`.
impl fmt::Display for KeymapWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(section) = self.section {
            write!(f, "section {}", section + 1)?;
            match &self.context {
                Some(context) => write!(f, " (context \"{context}\")")?,
                None => f.write_str(" (no context)")?,
            }
            if let Some(key) = &self.key {
                write!(f, ", binding \"{key}\"")?;
            }
            f.write_str(": ")?;
        }
        f.write_str(&self.message)
    }
}

impl Keymap {
    /// Parse a keymap file and add its bindings after the ones already loaded, so they win
    /// ties against them. Load the built-in keymap first and the user's after it.
    ///
    /// `text` is Zed's format: JSON with comments and trailing commas, an array of sections
    /// `{ "context": "<predicate>", "bindings": { "<keystrokes>": <action> } }`. A section
    /// without `"context"` (or with an empty one) applies everywhere. `<action>` is `"name"`,
    /// `["name", args]` or `null`, which disables the keystrokes; `"zed::NoAction"` is
    /// another way to write `null`. `["action::Sequence", [a, b, ...]]` runs several actions,
    /// each written like a binding's action. `["zed::Unbind", "name"]`, or an entry in the
    /// section's `"unbind"` object, disables the bindings of one action. Bindings keep the
    /// order they have in the file. `"use_key_equivalents"` is accepted and ignored.
    ///
    /// Nothing is fatal. A file that is not valid JSON or not an array loads nothing; a
    /// section that is not an object or whose context does not parse is skipped whole; a
    /// binding whose keystrokes or action do not parse is skipped on its own; an unknown
    /// field in a section is only reported. Each of these gives one warning. Action names are
    /// not checked, since only the app knows its actions. An empty file, or one with nothing
    /// but comments, is an empty keymap.
    pub fn load(&mut self, text: &str, source: KeymapSource) -> Vec<KeymapWarning> {
        let mut loader = Loader {
            bindings: &mut self.bindings,
            warnings: Vec::new(),
            source,
        };
        loader.load_file(text);
        loader.warnings
    }
}

/// The fields a section may have.
const SECTION_FIELDS: [&str; 4] = ["context", "use_key_equivalents", "unbind", "bindings"];

struct Loader<'a> {
    bindings: &'a mut Vec<Binding>,
    warnings: Vec<KeymapWarning>,
    source: KeymapSource,
}

/// The section a warning is about.
#[derive(Clone, Copy)]
struct Place<'a> {
    section: usize,
    context: Option<&'a str>,
}

impl Loader<'_> {
    fn warn_file(&mut self, message: String) {
        self.warnings.push(KeymapWarning {
            section: None,
            context: None,
            key: None,
            message,
        });
    }

    fn warn(&mut self, place: Place<'_>, key: Option<&str>, message: String) {
        self.warnings.push(KeymapWarning {
            section: Some(place.section),
            context: place.context.map(str::to_string),
            key: key.map(str::to_string),
            message,
        });
    }

    fn load_file(&mut self, text: &str) {
        let json = jsonc::strip(text.strip_prefix('\u{feff}').unwrap_or(text));
        if json.trim().is_empty() {
            return;
        }
        let value: Value = match serde_json::from_str(&json) {
            Ok(value) => value,
            Err(error) => return self.warn_file(format!("not valid JSON: {error}")),
        };
        let Value::Array(sections) = &value else {
            return self.warn_file("a keymap is an array of sections: [ { ... } ]".to_string());
        };
        // The same text again, this time only for the order of the keys.
        let shape: Shape = serde_json::from_str(&json).unwrap_or(Shape::Other);
        for (index, section) in sections.iter().enumerate() {
            self.load_section(index, section, shape.item(index));
        }
    }

    fn load_section(&mut self, index: usize, section: &Value, shape: Option<&Shape>) {
        let mut place = Place {
            section: index,
            context: None,
        };
        let Value::Object(fields) = section else {
            return self.warn(place, None, "a section is an object: { ... }".to_string());
        };
        let context = match fields.get("context") {
            None => None,
            Some(Value::String(context)) => {
                place.context = Some(context.as_str());
                match parse_context(context) {
                    Ok(predicate) => predicate,
                    Err(message) => return self.warn(place, None, message),
                }
            }
            Some(_) => {
                let message = "\"context\" must be a string".to_string();
                return self.warn(place, None, message);
            }
        };
        for (field, _) in ordered_entries(fields, shape) {
            if !SECTION_FIELDS.contains(&field) {
                self.warn(place, None, format!("unknown field \"{field}\""));
            }
        }
        // Zed reads a section's unbindings before its bindings, so a binding in the same
        // section can still take the keys back.
        for field in ["unbind", "bindings"] {
            match fields.get(field) {
                None => {}
                Some(Value::Object(entries)) => {
                    let shape = shape.and_then(|shape| shape.field(field));
                    for (keys, action) in ordered_entries(entries, shape) {
                        self.load_binding(place, field == "unbind", keys, action, context.as_ref());
                    }
                }
                Some(_) => {
                    let message = format!("\"{field}\" must be an object of keystrokes");
                    self.warn(place, None, message);
                }
            }
        }
    }

    fn load_binding(
        &mut self,
        place: Place<'_>,
        unbind: bool,
        keys: &str,
        action: &Value,
        context: Option<&Arc<ContextPredicate>>,
    ) {
        let keystrokes = match Keystroke::parse_sequence(keys) {
            Ok(keystrokes) => keystrokes,
            Err(error) => return self.warn(place, Some(keys), error.to_string()),
        };
        let target = if unbind {
            parse_unbind_target(action)
        } else {
            parse_target(action)
        };
        match target {
            Ok(target) => self.bindings.push(Binding {
                keystrokes,
                target,
                context: context.cloned(),
                source: self.source,
            }),
            Err(message) => self.warn(place, Some(keys), message),
        }
    }
}

/// A section's context: `None` for the empty string, which Zed takes as no context.
fn parse_context(context: &str) -> Result<Option<Arc<ContextPredicate>>, String> {
    if context.is_empty() {
        return Ok(None);
    }
    match ContextPredicate::parse(context) {
        Ok(predicate) => Ok(Some(Arc::new(predicate))),
        Err(error) => Err(format!("invalid context: {error}")),
    }
}

const EXPECTED_ACTION: &str = "an action is \"name\", [\"name\", args] or null";

/// Read a binding's action: `null`, `"name"`, `["name"]` or `["name", args]`, with the three
/// names the keymap itself understands (`zed::NoAction`, `zed::Unbind`, `action::Sequence`)
/// turned into what they mean. An argument of `null` is no argument.
pub(crate) fn parse_target(value: &Value) -> Result<Target, String> {
    let (name, input) = match value {
        Value::Null => return Ok(Target::Null),
        Value::String(name) => (name.as_str(), None),
        Value::Array(items) => match items.as_slice() {
            [Value::String(name)] => (name.as_str(), None),
            [Value::String(name), input] => (name.as_str(), Some(input)),
            _ => return Err(EXPECTED_ACTION.to_string()),
        },
        _ => return Err(EXPECTED_ACTION.to_string()),
    };
    let input = input.filter(|input| !input.is_null());
    match (name, input) {
        ("", _) => Err("the action name is empty".to_string()),
        (NO_ACTION, _) => Ok(Target::Null),
        (UNBIND, Some(Value::String(action))) if !action.is_empty() => {
            Ok(Target::Unbind(action.clone()))
        }
        (UNBIND, _) => Err(format!(
            "{UNBIND} takes the name of the action to unbind: [\"{UNBIND}\", \"name\"]"
        )),
        (SEQUENCE, Some(Value::Array(items))) => Ok(Target::Sequence(parse_sequence(items)?)),
        (SEQUENCE, _) => Err(format!(
            "{SEQUENCE} takes an array of actions: [\"{SEQUENCE}\", [\"a\", \"b\"]]"
        )),
        (name, input) => Ok(Target::Action(Action {
            name: name.to_string(),
            args: input.cloned(),
        })),
    }
}

/// The actions of an `action::Sequence`, nested sequences flattened in the order written.
/// The entries that run nothing (`null`, `zed::NoAction`, `zed::Unbind`) are dropped; Zed
/// dispatches them to no effect.
fn parse_sequence(items: &[Value]) -> Result<Vec<Action>, String> {
    let mut actions = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let target = parse_target(item)
            .map_err(|error| format!("in the sequence at index {index}: {error}"))?;
        match target {
            Target::Null | Target::Unbind(_) => {}
            Target::Action(action) => actions.push(action),
            Target::Sequence(nested) => actions.extend(nested),
        }
    }
    Ok(actions)
}

/// Read an entry of a section's `"unbind"` object: the action whose bindings for these
/// keystrokes are to be disabled. Only its name is kept.
fn parse_unbind_target(value: &Value) -> Result<Target, String> {
    match parse_target(value)? {
        Target::Action(action) => Ok(Target::Unbind(action.name)),
        Target::Sequence(_) => Ok(Target::Unbind(SEQUENCE.to_string())),
        Target::Null => {
            Err("an \"unbind\" entry names an action: \"name\" or [\"name\", args]".to_string())
        }
        Target::Unbind(_) => Err(format!("{UNBIND} cannot itself be unbound")),
    }
}

/// The entries of a JSON object in the order the file has them. A key written twice counts
/// once, at its first place and with its last value, which is what Zed's `IndexMap` makes of
/// it. Without a shape to go by the map's own order has to do.
fn ordered_entries<'a>(
    map: &'a Map<String, Value>,
    shape: Option<&'a Shape>,
) -> Vec<(&'a str, &'a Value)> {
    let Some(Shape::Object(keys)) = shape else {
        return map
            .iter()
            .map(|(key, value)| (key.as_str(), value))
            .collect();
    };
    let mut seen = HashSet::new();
    keys.iter()
        .filter(|(key, _)| seen.insert(key.as_str()))
        .filter_map(|(key, _)| Some((key.as_str(), map.get(key)?)))
        .collect()
}

/// The key order of the JSON objects in a document.
///
/// `serde_json::Value` keeps object keys sorted unless its `preserve_order` feature is on,
/// which depends on what else is in the build. The order of the bindings in a section
/// decides which wins, so it is read separately and explicitly.
enum Shape {
    /// A string, number, boolean or null.
    Other,
    Array(Vec<Shape>),
    Object(Vec<(String, Shape)>),
}

impl Shape {
    fn item(&self, index: usize) -> Option<&Shape> {
        match self {
            Shape::Array(items) => items.get(index),
            _ => None,
        }
    }

    /// The shape of an object's field. A field written twice has its last value, the one
    /// `serde_json::Value` keeps.
    fn field(&self, name: &str) -> Option<&Shape> {
        match self {
            Shape::Object(fields) => fields
                .iter()
                .rev()
                .find(|(key, _)| key == name)
                .map(|(_, shape)| shape),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Shape {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Shape, D::Error> {
        deserializer.deserialize_any(ShapeVisitor)
    }
}

struct ShapeVisitor;

impl<'de> Visitor<'de> for ShapeVisitor {
    type Value = Shape;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Shape, E> {
        Ok(Shape::Other)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Shape, E> {
        Ok(Shape::Other)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Shape, E> {
        Ok(Shape::Other)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Shape, E> {
        Ok(Shape::Other)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Shape, E> {
        Ok(Shape::Other)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Shape, E> {
        Ok(Shape::Other)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shape, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Shape::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Shape, A::Error> {
        let mut fields = Vec::new();
        while let Some(field) = map.next_entry::<String, Shape>()? {
            fields.push(field);
        }
        Ok(Shape::Object(fields))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn load(text: &str) -> (Keymap, Vec<KeymapWarning>) {
        let mut keymap = Keymap::new();
        let warnings = keymap.load(text, KeymapSource::User);
        (keymap, warnings)
    }

    fn texts(keymap: &Keymap) -> Vec<String> {
        keymap
            .bindings()
            .iter()
            .map(Binding::keystroke_text)
            .collect()
    }

    fn messages(warnings: &[KeymapWarning]) -> Vec<String> {
        warnings.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn loads_zed_format_with_comments_and_trailing_commas() {
        let (keymap, warnings) = load(
            r#"
            // Zed keymap
            [
              {
                "context": "Workspace",
                "use_key_equivalents": true,
                "bindings": {
                  // "shift shift": "file_finder::Toggle"
                  "cmd-t": "terminal_panel::Toggle",
                  /* block
                     comment */
                  "cmd-alt-g": ["action::Sequence", ["git::Diff", "workspace::ToggleZoom"]],
                },
              },
              {
                "bindings": { "cmd-k cmd-s": "zed::OpenKeymap", "f5": null },
              },
              { "context": "Editor && vim_mode == insert", "bindings": {} },
              { "context": "" , "bindings": { "enter": "menu::Confirm" } },
              {},
            ]
            "#,
        );
        assert_eq!(warnings, []);
        assert_eq!(
            texts(&keymap),
            ["cmd-t", "alt-cmd-g", "cmd-k cmd-s", "f5", "enter"]
        );
        let bindings = keymap.bindings();
        assert_eq!(
            bindings[0].context().map(ToString::to_string),
            Some("Workspace".to_string())
        );
        assert_eq!(
            bindings[0].actions(),
            [Action::new("terminal_panel::Toggle")]
        );
        assert_eq!(bindings[1].actions().len(), 2);
        assert_eq!(bindings[2].context(), None);
        assert!(bindings[3].is_null());
        // An empty context is no context.
        assert_eq!(bindings[4].context(), None);
        assert!(bindings
            .iter()
            .all(|binding| binding.source() == KeymapSource::User));
    }

    #[test]
    fn later_loads_are_added_after_earlier_ones() {
        let mut keymap = Keymap::new();
        assert_eq!(
            keymap.load(r#"[{"bindings": {"a": "First"}}]"#, KeymapSource::Default),
            []
        );
        assert_eq!(
            keymap.load(r#"[{"bindings": {"a": "Second"}}]"#, KeymapSource::User),
            []
        );
        let sources: Vec<KeymapSource> = keymap.bindings().iter().map(Binding::source).collect();
        assert_eq!(sources, [KeymapSource::Default, KeymapSource::User]);
        assert_eq!(keymap.bindings()[1].actions(), [Action::new("Second")]);
    }

    #[test]
    fn an_empty_file_is_an_empty_keymap() {
        for text in [
            "",
            "   \n\t",
            "// nothing here yet\n",
            "/* all of it\n commented out */",
            "[]",
            "[ // none\n]",
            "\u{feff}[]",
        ] {
            let (keymap, warnings) = load(text);
            assert_eq!(warnings, [], "{text:?}");
            assert!(keymap.is_empty(), "{text:?}");
        }
    }

    #[test]
    fn a_file_that_is_not_a_keymap_loads_nothing() {
        let (keymap, warnings) = load("[\n  {\"bindings\": {\"a\": }}\n]");
        assert!(keymap.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].section, None);
        assert_eq!(warnings[0].key, None);
        let message = warnings[0].to_string();
        assert!(message.starts_with("not valid JSON: "), "{message}");
        assert!(message.contains("line 2"), "{message}");

        for text in [r#"{"bindings": {"a": "b"}}"#, "\"keymap\"", "42", "null"] {
            let (keymap, warnings) = load(text);
            assert!(keymap.is_empty());
            assert_eq!(
                messages(&warnings),
                ["a keymap is an array of sections: [ { ... } ]"],
                "{text}"
            );
        }

        // Too deeply nested to read is an error like any other, not a crash.
        let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        let (keymap, warnings) = load(&deep);
        assert!(keymap.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn a_bad_section_is_skipped_and_the_rest_loads() {
        let (keymap, warnings) = load(
            r#"[
                {"context": "Editor", "bindings": {"a": "One"}},
                "not a section",
                {"context": "Editor &&", "bindings": {"b": "Two"}},
                {"context": 7, "bindings": {"c": "Three"}},
                {"context": "Pane", "bindings": ["d", "Four"]},
                {"context": "Pane", "unbind": "e"},
                {"context": "   ", "bindings": {"f": "Six"}},
                {"context": "Terminal", "bindings": {"g": "Seven"}}
            ]"#,
        );
        assert_eq!(texts(&keymap), ["a", "g"]);
        assert_eq!(
            messages(&warnings),
            [
                "section 2 (no context): a section is an object: { ... }",
                "section 3 (context \"Editor &&\"): invalid context: unexpected end of the \
                 expression at column 10",
                "section 4 (no context): \"context\" must be a string",
                "section 5 (context \"Pane\"): \"bindings\" must be an object of keystrokes",
                "section 6 (context \"Pane\"): \"unbind\" must be an object of keystrokes",
                "section 7 (context \"   \"): invalid context: unexpected end of the expression \
                 at column 4",
            ]
        );
        assert_eq!(warnings[1].section, Some(2));
        assert_eq!(warnings[1].context.as_deref(), Some("Editor &&"));
        assert_eq!(warnings[1].key, None);
    }

    #[test]
    fn a_bad_binding_is_skipped_on_its_own() {
        let (keymap, warnings) = load(
            r#"[{"context": "Editor", "bindings": {
                "cmd-k": "Good",
                "cmd-nope": "BadKey",
                "cmd-k a-b": "BadSecondKey",
                "shift shift": "file_finder::Toggle",
                "": "NoKeys",
                "cmd-1": 5,
                "cmd-2": {"name": "x"},
                "cmd-3": [],
                "cmd-4": ["a", 1, 2],
                "cmd-5": [1, "a"],
                "cmd-6": "",
                "cmd-7": "action::Sequence",
                "cmd-8": ["action::Sequence", "a"],
                "cmd-9": ["action::Sequence", ["a", 3]],
                "alt-1": "zed::Unbind",
                "alt-2": ["zed::Unbind", {"name": "a"}],
                "alt-3": true,
                "cmd-j": "AlsoGood"
            }}]"#,
        );
        assert_eq!(texts(&keymap), ["cmd-k", "cmd-j"]);
        let found: Vec<(&str, &str)> = warnings
            .iter()
            .map(|w| (w.key.as_deref().unwrap(), w.message.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (
                    "cmd-nope",
                    "invalid keystroke \"cmd-nope\": unknown key \"nope\""
                ),
                (
                    "cmd-k a-b",
                    "invalid keystroke \"a-b\": unknown key \"a-b\""
                ),
                (
                    "shift shift",
                    "invalid keystroke \"shift\": \"shift\" is a modifier, and a modifier on its \
                     own cannot be bound"
                ),
                ("", "invalid keystroke \"\": there is no keystroke"),
                ("cmd-1", EXPECTED_ACTION),
                ("cmd-2", EXPECTED_ACTION),
                ("cmd-3", EXPECTED_ACTION),
                ("cmd-4", EXPECTED_ACTION),
                ("cmd-5", EXPECTED_ACTION),
                ("cmd-6", "the action name is empty"),
                (
                    "cmd-7",
                    "action::Sequence takes an array of actions: [\"action::Sequence\", [\"a\", \
                     \"b\"]]"
                ),
                (
                    "cmd-8",
                    "action::Sequence takes an array of actions: [\"action::Sequence\", [\"a\", \
                     \"b\"]]"
                ),
                (
                    "cmd-9",
                    "in the sequence at index 1: an action is \"name\", [\"name\", args] or null"
                ),
                (
                    "alt-1",
                    "zed::Unbind takes the name of the action to unbind: [\"zed::Unbind\", \
                     \"name\"]"
                ),
                (
                    "alt-2",
                    "zed::Unbind takes the name of the action to unbind: [\"zed::Unbind\", \
                     \"name\"]"
                ),
                ("alt-3", EXPECTED_ACTION),
            ]
        );
        assert!(warnings.iter().all(|w| w.section == Some(0)));
        assert!(warnings
            .iter()
            .all(|w| w.context.as_deref() == Some("Editor")));
        assert_eq!(
            warnings[0].to_string(),
            "section 1 (context \"Editor\"), binding \"cmd-nope\": invalid keystroke \
             \"cmd-nope\": unknown key \"nope\""
        );
    }

    #[test]
    fn unknown_fields_are_reported_and_the_section_still_loads() {
        let (keymap, warnings) = load(
            r#"[{"zebra": 1, "binding": {"a": "Typo"}, "bindings": {"b": "Kept"}, "apple": []}]"#,
        );
        assert_eq!(texts(&keymap), ["b"]);
        // In the order the file has them, not sorted.
        assert_eq!(
            messages(&warnings),
            [
                "section 1 (no context): unknown field \"zebra\"",
                "section 1 (no context): unknown field \"binding\"",
                "section 1 (no context): unknown field \"apple\"",
            ]
        );
    }

    #[test]
    fn unbind_entries_must_name_an_action() {
        let (keymap, warnings) = load(
            r#"[{"context": "Editor", "unbind": {
                "tab": "editor::Tab",
                "enter": ["editor::Newline", {"x": 1}],
                "cmd-g": ["action::Sequence", ["a", "b"]],
                "escape": null,
                "space": ["zed::Unbind", "a"],
                "nope-x": "editor::Tab",
                "f1": 3
            }}]"#,
        );
        let unbound: Vec<(String, Option<&str>)> = keymap
            .bindings()
            .iter()
            .map(|binding| (binding.keystroke_text(), binding.unbinds()))
            .collect();
        assert_eq!(
            unbound,
            [
                ("tab".to_string(), Some("editor::Tab")),
                ("enter".to_string(), Some("editor::Newline")),
                ("cmd-g".to_string(), Some("action::Sequence")),
            ]
        );
        assert_eq!(
            messages(&warnings),
            [
                "section 1 (context \"Editor\"), binding \"escape\": an \"unbind\" entry names an \
                 action: \"name\" or [\"name\", args]",
                "section 1 (context \"Editor\"), binding \"space\": zed::Unbind cannot itself be \
                 unbound",
                "section 1 (context \"Editor\"), binding \"nope-x\": invalid keystroke \
                 \"nope-x\": unknown key \"nope-x\"",
                "section 1 (context \"Editor\"), binding \"f1\": an action is \"name\", \
                 [\"name\", args] or null",
            ]
        );
    }

    #[test]
    fn a_key_written_twice_keeps_its_first_place_and_last_value() {
        let (keymap, warnings) =
            load(r#"[{"bindings": {"b": "First", "a": "Middle", "b": "Last"}}]"#);
        assert_eq!(warnings, []);
        assert_eq!(texts(&keymap), ["b", "a"]);
        assert_eq!(keymap.bindings()[0].actions(), [Action::new("Last")]);
        // The same for a whole field of a section.
        let (keymap, warnings) = load(
            r#"[{"bindings": {"z": "Dropped"}, "context": "Editor", "bindings": {"y": "Kept", "x": "Kept"}}]"#,
        );
        assert_eq!(warnings, []);
        assert_eq!(texts(&keymap), ["y", "x"]);
    }

    #[test]
    fn parses_every_shape_of_action() {
        let action = |value: Value| parse_target(&value);
        assert_eq!(action(json!(null)), Ok(Target::Null));
        assert_eq!(action(json!("zed::NoAction")), Ok(Target::Null));
        assert_eq!(action(json!(["zed::NoAction", 1])), Ok(Target::Null));
        assert_eq!(
            action(json!("editor::Copy")),
            Ok(Target::Action(Action::new("editor::Copy")))
        );
        assert_eq!(
            action(json!(["editor::Copy"])),
            Ok(Target::Action(Action::new("editor::Copy")))
        );
        assert_eq!(
            action(json!(["editor::Copy", null])),
            Ok(Target::Action(Action::new("editor::Copy")))
        );
        assert_eq!(
            action(json!(["pane::ActivateItem", 3])),
            Ok(Target::Action(Action::with_args(
                "pane::ActivateItem",
                json!(3)
            )))
        );
        assert_eq!(
            action(json!(["task::Spawn", {"task_name": "x"}])),
            Ok(Target::Action(Action::with_args(
                "task::Spawn",
                json!({"task_name": "x"})
            )))
        );
        assert_eq!(
            action(json!(["zed::Unbind", "editor::Tab"])),
            Ok(Target::Unbind("editor::Tab".to_string()))
        );
        assert_eq!(
            action(json!(["action::Sequence", ["a", ["b", 1]]])),
            Ok(Target::Sequence(vec![
                Action::new("a"),
                Action::with_args("b", json!(1))
            ]))
        );
        assert_eq!(
            action(json!(["action::Sequence", []])),
            Ok(Target::Sequence(Vec::new()))
        );
        assert!(action(json!(["zed::Unbind", ""])).is_err());
        assert!(action(json!(["action::Sequence", null])).is_err());
        assert!(action(json!(["action::Sequence", ["a", ["action::Sequence", 4]]])).is_err());
    }

    #[test]
    fn key_order_is_read_from_the_text() {
        let shape: Shape =
            serde_json::from_str(r#"[{"z": 1, "a": {"y": [true, null, 1.5, -2], "b": "s"}}, 7]"#)
                .unwrap();
        let Some(Shape::Object(fields)) = shape.item(0) else {
            panic!("an object expected");
        };
        let keys: Vec<&str> = fields.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["z", "a"]);
        let Some(Shape::Object(inner)) = shape.item(0).and_then(|s| s.field("a")) else {
            panic!("an object expected");
        };
        let keys: Vec<&str> = inner.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["y", "b"]);
        assert!(matches!(shape.item(1), Some(Shape::Other)));
        assert!(shape.item(2).is_none());
        assert!(shape.field("z").is_none());
        assert!(matches!(inner[0].1.item(0), Some(Shape::Other)));
    }
}
