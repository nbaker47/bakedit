//! Named actions: what a key binding, a palette entry or a mouse click asks the app to do.
//!
//! Names follow Zed (`editor::MoveUp`, `terminal_panel::Toggle`) so a Zed keymap reads the
//! same here. Arguments stay as JSON, exactly as written in the keymap.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub name: String,
    pub args: Option<Value>,
}

impl Action {
    pub fn new(name: impl Into<String>) -> Action {
        Action { name: name.into(), args: None }
    }

    pub fn with_args(name: impl Into<String>, args: Value) -> Action {
        Action { name: name.into(), args: Some(args) }
    }

    /// Read an action from a keymap value: `"name"`, `["name"]`, `["name", args]`.
    /// `null` (an unbinding) and anything malformed come back as `None`.
    pub fn from_json(value: &Value) -> Option<Action> {
        match value {
            Value::String(name) if !name.is_empty() => Some(Action::new(name.clone())),
            Value::Array(items) => {
                let name = items.first()?.as_str()?;
                if name.is_empty() || items.len() > 2 {
                    return None;
                }
                Some(Action { name: name.to_string(), args: items.get(1).cloned() })
            }
            _ => None,
        }
    }

    /// The part before `::` (`editor` in `editor::MoveUp`), or "" when there is none.
    pub fn namespace(&self) -> &str {
        self.name.split_once("::").map(|(ns, _)| ns).unwrap_or("")
    }

    /// A string argument: the whole argument when it is a string, or field `key` of an object.
    pub fn arg_str(&self, key: &str) -> Option<&str> {
        match self.args.as_ref()? {
            Value::String(s) => Some(s),
            Value::Object(map) => map.get(key)?.as_str(),
            _ => None,
        }
    }

    /// An integer argument: the whole argument when it is a number, or field `key` of an object.
    pub fn arg_i64(&self, key: &str) -> Option<i64> {
        match self.args.as_ref()? {
            Value::Number(n) => n.as_i64(),
            Value::Object(map) => map.get(key)?.as_i64(),
            _ => None,
        }
    }

    /// A boolean argument: field `key` of an object argument.
    pub fn arg_bool(&self, key: &str) -> Option<bool> {
        self.args.as_ref()?.as_object()?.get(key)?.as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_the_three_keymap_shapes() {
        assert_eq!(Action::from_json(&json!("editor::MoveUp")), Some(Action::new("editor::MoveUp")));
        assert_eq!(Action::from_json(&json!(["editor::Copy"])), Some(Action::new("editor::Copy")));
        let a = Action::from_json(&json!(["editor::MoveUpByLines", {"lines": 5}])).unwrap();
        assert_eq!(a.name, "editor::MoveUpByLines");
        assert_eq!(a.arg_i64("lines"), Some(5));
    }

    #[test]
    fn null_and_malformed_are_none() {
        assert_eq!(Action::from_json(&Value::Null), None);
        assert_eq!(Action::from_json(&json!("")), None);
        assert_eq!(Action::from_json(&json!([])), None);
        assert_eq!(Action::from_json(&json!([1, 2])), None);
        assert_eq!(Action::from_json(&json!(["a", 1, 2])), None);
    }

    #[test]
    fn scalar_arguments_ignore_the_key() {
        let a = Action::from_json(&json!(["terminal::SendText", "\u{2}h"])).unwrap();
        assert_eq!(a.arg_str("text"), Some("\u{2}h"));
        let a = Action::from_json(&json!(["pane::ActivateItem", 3])).unwrap();
        assert_eq!(a.arg_i64("index"), Some(3));
        assert_eq!(a.namespace(), "pane");
    }
}
