//! The clipboard: the system one when there is one, with enough memory of our own to know
//! that the text on it was copied as whole lines.
//!
//! Copying with nothing selected takes the whole line (as in Zed and VS Code), and pasting
//! that should put the line above the cursor rather than in the middle of a word. The
//! system clipboard cannot carry that flag, so we remember the last text we put there and
//! treat a paste as line-wise only when the clipboard still holds exactly that text.

/// Where clipboard text is kept.
pub trait Backend {
    fn get(&mut self) -> Option<String>;
    /// Returns false when the text could not be stored.
    fn set(&mut self, text: &str) -> bool;
}

/// The operating system's clipboard.
pub struct SystemBackend {
    inner: arboard::Clipboard,
}

impl SystemBackend {
    pub fn new() -> Option<SystemBackend> {
        arboard::Clipboard::new().ok().map(|inner| SystemBackend { inner })
    }
}

impl Backend for SystemBackend {
    fn get(&mut self) -> Option<String> {
        self.inner.get_text().ok()
    }

    fn set(&mut self, text: &str) -> bool {
        self.inner.set_text(text.to_string()).is_ok()
    }
}

/// A clipboard that lives only inside this process: for tests, and for sessions with no
/// system clipboard to reach.
#[derive(Default)]
pub struct MemoryBackend {
    text: Option<String>,
}

impl Backend for MemoryBackend {
    fn get(&mut self) -> Option<String> {
        self.text.clone()
    }

    fn set(&mut self, text: &str) -> bool {
        self.text = Some(text.to_string());
        true
    }
}

/// Text taken from the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    pub text: String,
    /// The text was copied as whole lines by this editor and should paste as lines.
    pub linewise: bool,
}

pub struct Clipboard {
    backend: Box<dyn Backend>,
    /// The last text we set, and whether it was line-wise.
    last: Option<(String, bool)>,
}

impl Clipboard {
    /// The system clipboard, or an in-process one when `BAKEDIT_CLIPBOARD=memory` is set
    /// (tests use that so they never overwrite what the user has copied) or the system one
    /// cannot be opened.
    pub fn new() -> Clipboard {
        let memory = std::env::var("BAKEDIT_CLIPBOARD").is_ok_and(|v| v == "memory");
        let backend: Box<dyn Backend> = match SystemBackend::new() {
            Some(system) if !memory => Box::new(system),
            _ => Box::new(MemoryBackend::default()),
        };
        Clipboard { backend, last: None }
    }

    pub fn with_backend(backend: Box<dyn Backend>) -> Clipboard {
        Clipboard { backend, last: None }
    }

    /// Put text on the clipboard. Returns false if the system refused it; the text is then
    /// still available to our own pastes.
    pub fn set(&mut self, text: &str, linewise: bool) -> bool {
        self.last = Some((text.to_string(), linewise));
        self.backend.set(text)
    }

    /// What a paste should insert, or `None` when the clipboard holds no text.
    pub fn get(&mut self) -> Option<Clip> {
        match self.backend.get() {
            Some(text) => {
                let linewise = matches!(&self.last, Some((last, true)) if *last == text);
                Some(Clip { text, linewise })
            }
            // The system clipboard is unreachable or empty: fall back to our own copy.
            None => self.last.clone().map(|(text, linewise)| Clip { text, linewise }),
        }
    }
}

impl Default for Clipboard {
    fn default() -> Clipboard {
        Clipboard::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A backend whose contents the test can change behind the clipboard's back, like
    /// another application copying something.
    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Option<String>>>);

    impl Backend for Shared {
        fn get(&mut self) -> Option<String> {
            self.0.borrow().clone()
        }
        fn set(&mut self, text: &str) -> bool {
            *self.0.borrow_mut() = Some(text.to_string());
            true
        }
    }

    struct Broken;

    impl Backend for Broken {
        fn get(&mut self) -> Option<String> {
            None
        }
        fn set(&mut self, _: &str) -> bool {
            false
        }
    }

    #[test]
    fn remembers_that_a_copy_was_linewise() {
        let mut clipboard = Clipboard::with_backend(Box::new(MemoryBackend::default()));
        assert_eq!(clipboard.get(), None);
        clipboard.set("let x = 1;\n", true);
        assert_eq!(clipboard.get(), Some(Clip { text: "let x = 1;\n".into(), linewise: true }));
        clipboard.set("word", false);
        assert_eq!(clipboard.get(), Some(Clip { text: "word".into(), linewise: false }));
    }

    #[test]
    fn text_copied_elsewhere_is_not_linewise() {
        let shared = Shared::default();
        let mut clipboard = Clipboard::with_backend(Box::new(shared.clone()));
        clipboard.set("a line\n", true);
        *shared.0.borrow_mut() = Some("from the browser".to_string());
        assert_eq!(clipboard.get(), Some(Clip { text: "from the browser".into(), linewise: false }));
        // Even identical-looking text copied after a non-linewise copy stays character-wise.
        clipboard.set("x", false);
        *shared.0.borrow_mut() = Some("a line\n".to_string());
        assert!(!clipboard.get().unwrap().linewise);
    }

    #[test]
    fn works_without_a_system_clipboard() {
        let mut clipboard = Clipboard::with_backend(Box::new(Broken));
        assert_eq!(clipboard.get(), None);
        assert!(!clipboard.set("kept anyway", true));
        assert_eq!(clipboard.get(), Some(Clip { text: "kept anyway".into(), linewise: true }));
    }
}
