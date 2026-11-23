//! Languages: what a file is, and the editing conventions that follow from it.

use std::path::Path;

use crate::grammar::{self, GrammarSpec};
use crate::registry::LANGUAGES;

/// An opening delimiter and the closing one that answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pair {
    /// The opening character, such as `(`.
    pub open: char,
    /// The closing character, such as `)`. Quotes close with the character that opened them.
    pub close: char,
}

impl Pair {
    /// A pair of delimiters.
    pub const fn new(open: char, close: char) -> Pair {
        Pair { open, close }
    }
}

/// One language the editor knows: how to recognise its files and how to edit them.
///
/// Languages are static data. Get one from [`language_for_path`], [`detect`],
/// [`language_by_id`] or [`languages`]; compare them with `==`, which compares ids.
#[derive(Debug)]
pub struct Language {
    /// A stable lower-case identifier, such as `rust` or `git_commit`.
    pub id: &'static str,
    /// The name shown to the user, as Zed spells it: `Rust`, `Shell Script`, `Git Commit`.
    pub name: &'static str,
    /// File extensions without the dot, lower-case, matched ignoring case. An entry may
    /// contain dots to match a compound extension (`d.ts`).
    pub extensions: &'static [&'static str],
    /// Whole file names (`Makefile`, `.zshrc`, `Cargo.lock`), matched exactly first and then
    /// ignoring case. An entry ending in `*` matches any name with that prefix
    /// (`Dockerfile.*`).
    pub file_names: &'static [&'static str],
    /// Interpreter names a `#!` line can name, without a version suffix (`python`, `bash`).
    pub interpreters: &'static [&'static str],
    /// Other names the language goes by in a fenced code block or an injection (`js`, `sh`).
    pub aliases: &'static [&'static str],
    /// What starts a comment that runs to the end of the line, with the space that follows
    /// it when a line is commented out (`"// "`, `"# "`, `"-- "`).
    pub line_comment: Option<&'static str>,
    /// What opens and closes a block comment, with the padding used when a selection is
    /// commented out (`("/* ", " */")`).
    pub block_comment: Option<(&'static str, &'static str)>,
    /// The bracket pairs to match and jump between.
    pub brackets: &'static [Pair],
    /// The pairs whose closer is typed for the user when they type the opener: the brackets
    /// plus the quotes that make sense in the language. Rust leaves out the single quote
    /// because of lifetimes; prose leaves out both quotes because of apostrophes.
    pub autoclose: &'static [Pair],
    /// Columns per indent level.
    pub tab_size: u8,
    /// Indent with tab characters instead of spaces.
    pub hard_tabs: bool,
    /// Wrap long lines at the editor's width. True for prose: Markdown, plain text, commit
    /// messages.
    pub soft_wrap: bool,
    /// Characters that make the next line one level deeper when a line ends with one of
    /// them (ignoring trailing white space): `{[(` in most languages, plus `:` in Python.
    pub indent_after: &'static str,
    /// Characters that count as part of a word beyond letters, digits and `_`: `$` in
    /// JavaScript, `-` in CSS.
    pub word_chars: &'static str,
    pub(crate) grammar: Option<&'static GrammarSpec>,
}

impl PartialEq for Language {
    fn eq(&self, other: &Language) -> bool {
        self.id == other.id
    }
}

impl Eq for Language {}

impl Language {
    /// Whether the language can be highlighted. Languages without a grammar still carry
    /// their comment, bracket and indent conventions.
    pub fn has_grammar(&self) -> bool {
        self.grammar.is_some()
    }

    /// Every capture name highlighting this language can produce, normalised and sorted.
    /// Empty when the language has no grammar. This loads the grammar if it is not loaded
    /// yet, and does not include the captures of languages injected into it.
    pub fn capture_names(&self) -> Vec<&'static str> {
        let Some(spec) = self.grammar else {
            return Vec::new();
        };
        let Ok(grammar) = grammar::load(spec) else {
            return Vec::new();
        };
        let mut names: Vec<&'static str> = grammar
            .capture_ids
            .iter()
            .flatten()
            .map(|id| id.name())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Compile the language's outline rules now instead of on the first [`outline`] call.
    ///
    /// [`outline`]: crate::outline
    pub(crate) fn prepare_outline(&self) {
        if let Some(spec) = self.grammar {
            spec.outline_query();
        }
    }

    /// Whether `ch` is part of a word in this language: a letter, a digit, `_`, or one of
    /// [`Language::word_chars`].
    pub fn is_word_char(&self, ch: char) -> bool {
        ch.is_alphanumeric() || ch == '_' || self.word_chars.contains(ch)
    }
}

/// Every language, plain text first.
pub fn languages() -> &'static [Language] {
    &LANGUAGES
}

/// The language with this id (`rust`, `plain_text`), or `None`.
pub fn language_by_id(id: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|language| language.id == id)
}

/// The language for text that belongs to no other: no grammar, soft-wrapped.
pub fn plain_text() -> &'static Language {
    &LANGUAGES[0]
}

/// The language a name in a fenced code block or an injection refers to: an id, an alias, a
/// display name or a file extension, ignoring case (`rust`, `rs`, `JS`, `c++`, `Shell
/// Script`). `None` when nothing matches.
pub fn language_by_name(name: &str) -> Option<&'static Language> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let is = |candidate: &&str| candidate.eq_ignore_ascii_case(name);
    LANGUAGES
        .iter()
        .find(|l| is(&l.id) || l.aliases.iter().any(is))
        .or_else(|| LANGUAGES.iter().find(|l| is(&l.name)))
        .or_else(|| LANGUAGES.iter().find(|l| l.extensions.iter().any(is)))
}

/// The language of a file, from its name, its extension or its first line.
///
/// The checks run from most to least specific: the whole file name (`Makefile`,
/// `Cargo.lock`, `.zshrc`), then the extension (the longest matching one, ignoring case),
/// then a `#!` line in `first_line`, then the same again with the decorations of a template
/// or a chezmoi source file removed (`config.toml.tmpl`, `dot_zshrc`,
/// `executable_backup.sh`). Only the last component of `path` is looked at, and the file is
/// never opened.
///
/// Returns `None` when nothing matches; [`detect`] turns that into plain text.
pub fn language_for_path(path: &Path, first_line: Option<&str>) -> Option<&'static Language> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    by_file_name(name)
        .or_else(|| by_extension(name))
        .or_else(|| first_line.and_then(by_shebang))
        .or_else(|| {
            let plain = undecorated(name)?;
            by_file_name(&plain).or_else(|| by_extension(&plain))
        })
}

/// The language of a file, falling back to [`plain_text`] when [`language_for_path`] finds
/// nothing.
pub fn detect(path: &Path, first_line: Option<&str>) -> &'static Language {
    language_for_path(path, first_line).unwrap_or_else(plain_text)
}

/// Match a whole file name: exactly, then ignoring case, then against the `prefix*` entries.
fn by_file_name(name: &str) -> Option<&'static Language> {
    if name.is_empty() {
        return None;
    }
    let exact = |language: &&Language| language.file_names.contains(&name);
    let folded = |language: &&Language| {
        language
            .file_names
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
    };
    let prefixed = |language: &&Language| {
        language.file_names.iter().any(|candidate| {
            candidate.strip_suffix('*').is_some_and(|prefix| {
                name.len() > prefix.len()
                    && name.is_char_boundary(prefix.len())
                    && name[..prefix.len()].eq_ignore_ascii_case(prefix)
            })
        })
    };
    LANGUAGES
        .iter()
        .find(exact)
        .or_else(|| LANGUAGES.iter().find(folded))
        .or_else(|| LANGUAGES.iter().find(prefixed))
}

/// Match the extension, trying the longest candidate first so `x.d.ts` can be told from
/// `x.ts`. A leading dot is part of the name, not the start of an extension: `.zshrc` has
/// none.
fn by_extension(name: &str) -> Option<&'static Language> {
    let body = name.strip_prefix('.').unwrap_or(name);
    let mut rest = body;
    while let Some(dot) = rest.find('.') {
        rest = &rest[dot + 1..];
        if rest.is_empty() {
            return None;
        }
        let found = LANGUAGES.iter().find(|language| {
            language
                .extensions
                .iter()
                .any(|extension| extension.eq_ignore_ascii_case(rest))
        });
        if found.is_some() {
            return found;
        }
    }
    None
}

/// Match the interpreter named on a `#!` line: `#!/bin/sh`, `#!/usr/bin/env python3`,
/// `#!/usr/bin/env -S deno run`.
fn by_shebang(first_line: &str) -> Option<&'static Language> {
    let interpreter = shebang_interpreter(first_line)?;
    LANGUAGES
        .iter()
        .find(|language| language.interpreters.contains(&interpreter.as_str()))
}

/// The program a `#!` line runs, lower-cased and without its directory or version:
/// `python` for `#!/usr/local/bin/python3.12`. With `env`, the program is the first argument
/// that is neither an option nor a `NAME=value` assignment.
pub(crate) fn shebang_interpreter(first_line: &str) -> Option<String> {
    let command = first_line
        .trim_start_matches('\u{feff}')
        .strip_prefix("#!")?;
    let mut words = command.split_whitespace();
    let mut program = base_name(words.next()?);
    if program == "env" {
        program = base_name(words.find(|word| !word.starts_with('-') && !word.contains('='))?);
    }
    let program = program.to_ascii_lowercase();
    // `python3.12` and `ruby3` are still python and ruby; a name that is all digits stays.
    let stem = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    Some(if stem.is_empty() {
        program
    } else {
        stem.trim_end_matches('-').to_owned()
    })
}

/// The last component of a slash-separated path.
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Prefixes chezmoi puts on a source file's name to record the target's attributes.
const CHEZMOI_PREFIXES: &[&str] = &[
    "after_",
    "before_",
    "create_",
    "empty_",
    "encrypted_",
    "exact_",
    "executable_",
    "external_",
    "literal_",
    "modify_",
    "once_",
    "onchange_",
    "private_",
    "readonly_",
    "remove_",
    "run_",
    "symlink_",
];

/// The name a template or a chezmoi source file stands for: `config.toml` for
/// `config.toml.tmpl`, `.zshrc` for `private_dot_zshrc`. `None` when the name carries no
/// such decoration.
fn undecorated(name: &str) -> Option<String> {
    let mut rest = name;
    for suffix in [".tmpl", ".literal"] {
        rest = rest.strip_suffix(suffix).unwrap_or(rest);
    }
    while let Some(stripped) = CHEZMOI_PREFIXES
        .iter()
        .find_map(|prefix| rest.strip_prefix(prefix))
    {
        rest = stripped;
    }
    let plain = match rest.strip_prefix("dot_") {
        Some(hidden) => format!(".{hidden}"),
        None => rest.to_owned(),
    };
    (plain != name && !plain.is_empty()).then_some(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_for(path: &str) -> Option<&'static str> {
        language_for_path(Path::new(path), None).map(|language| language.id)
    }

    fn id_for_line(path: &str, first_line: &str) -> Option<&'static str> {
        language_for_path(Path::new(path), Some(first_line)).map(|language| language.id)
    }

    #[test]
    fn detects_by_extension() {
        let cases = [
            ("src/main.rs", "rust"),
            ("a/b/script.py", "python"),
            ("types.pyi", "python"),
            ("index.js", "javascript"),
            ("index.mjs", "javascript"),
            ("index.cjs", "javascript"),
            ("App.jsx", "javascript"),
            ("app.ts", "typescript"),
            ("app.mts", "typescript"),
            ("types.d.ts", "typescript"),
            ("App.tsx", "tsx"),
            ("package.json", "json"),
            ("settings.jsonc", "json"),
            ("Cargo.toml", "toml"),
            ("deploy.yaml", "yaml"),
            ("deploy.yml", "yaml"),
            ("README.md", "markdown"),
            ("notes.markdown", "markdown"),
            ("run.sh", "shell"),
            ("run.bash", "shell"),
            ("run.zsh", "shell"),
            ("main.go", "go"),
            ("main.c", "c"),
            ("main.h", "c"),
            ("main.cpp", "cpp"),
            ("main.cc", "cpp"),
            ("main.hpp", "cpp"),
            ("index.html", "html"),
            ("index.htm", "html"),
            ("style.css", "css"),
            ("App.swift", "swift"),
            ("Main.java", "java"),
            ("init.lua", "lua"),
            ("main.tf", "terraform"),
            ("vars.tfvars", "terraform"),
            ("config.hcl", "hcl"),
            ("query.sql", "sql"),
            ("player.gd", "gdscript"),
            ("app.rb", "ruby"),
            ("Main.kt", "kotlin"),
            ("build.gradle.kts", "kotlin"),
            ("fix.diff", "diff"),
            ("fix.patch", "diff"),
            ("rules.mk", "make"),
            ("flake.nix", "nix"),
            ("build.zig", "zig"),
            ("index.php", "php"),
            ("Program.cs", "csharp"),
            ("main.dart", "dart"),
            ("App.vue", "vue"),
            ("App.svelte", "svelte"),
            ("notes.txt", "plain_text"),
        ];
        for (path, expected) in cases {
            assert_eq!(id_for(path), Some(expected), "{path}");
        }
    }

    #[test]
    fn extensions_ignore_case_and_take_the_longest_match() {
        assert_eq!(id_for("README.MD"), Some("markdown"));
        assert_eq!(id_for("MAIN.RS"), Some("rust"));
        assert_eq!(id_for("archive.tar.rs"), Some("rust"));
        assert_eq!(id_for("a.b.c.json"), Some("json"));
        assert_eq!(id_for("no_extension"), None);
        assert_eq!(id_for("trailing."), None);
        assert_eq!(id_for("weird.unknownext"), None);
        assert_eq!(id_for(""), None);
        // A dotfile's name is not an extension.
        assert_eq!(id_for(".rs"), None);
        assert_eq!(id_for(".config.toml"), Some("toml"));
    }

    #[test]
    fn detects_by_exact_file_name() {
        let cases = [
            ("Dockerfile", "dockerfile"),
            ("docker/Dockerfile.dev", "dockerfile"),
            ("Containerfile", "dockerfile"),
            ("Makefile", "make"),
            ("makefile", "make"),
            ("GNUmakefile", "make"),
            ("Justfile", "just"),
            ("justfile", "just"),
            (".justfile", "just"),
            ("Cargo.lock", "toml"),
            ("uv.lock", "toml"),
            ("poetry.lock", "toml"),
            (".zshrc", "shell"),
            (".bashrc", "shell"),
            (".zprofile", "shell"),
            (".zshenv", "shell"),
            (".bash_profile", "shell"),
            (".profile", "shell"),
            (".env", "shell"),
            (".env.local", "shell"),
            (".gitignore", "gitignore"),
            (".dockerignore", "gitignore"),
            ("go.mod", "gomod"),
            ("COMMIT_EDITMSG", "git_commit"),
            ("MERGE_MSG", "git_commit"),
            ("Gemfile", "ruby"),
            ("Rakefile", "ruby"),
            ("Brewfile", "ruby"),
            ("Podfile", "ruby"),
            (".clang-format", "yaml"),
            (".prettierrc", "json"),
            ("flake.lock", "json"),
            ("LICENSE", "plain_text"),
        ];
        for (path, expected) in cases {
            assert_eq!(id_for(path), Some(expected), "{path}");
        }
    }

    #[test]
    fn a_file_name_beats_an_extension_and_case_is_a_tiebreak() {
        // `Cargo.lock` would otherwise have the unknown extension `lock`.
        assert_eq!(id_for("Cargo.lock"), Some("toml"));
        assert_eq!(id_for("dockerfile"), Some("dockerfile"));
        assert_eq!(id_for("MAKEFILE"), Some("make"));
        // The prefix form needs something after the prefix.
        assert_eq!(id_for("Dockerfile.prod"), Some("dockerfile"));
        assert_eq!(id_for(".env.production"), Some("shell"));
        // Only the last path component counts.
        assert_eq!(id_for("Makefile/readme.md"), Some("markdown"));
    }

    #[test]
    fn detects_by_shebang() {
        let cases = [
            ("#!/bin/sh", "shell"),
            ("#!/bin/bash", "shell"),
            ("#!/usr/bin/env bash", "shell"),
            ("#!/usr/bin/env zsh", "shell"),
            ("#! /bin/zsh -f", "shell"),
            ("#!/usr/bin/python", "python"),
            ("#!/usr/bin/env python3", "python"),
            ("#!/usr/local/bin/python3.12", "python"),
            ("#!/usr/bin/env -S python3 -u", "python"),
            ("#!/usr/bin/env -S uv run --script", "python"),
            ("#!/usr/bin/env node", "javascript"),
            ("#!/usr/bin/env -S deno run --allow-net", "javascript"),
            ("#!/usr/bin/env bun", "javascript"),
            ("#!/usr/bin/env ts-node", "typescript"),
            ("#!/usr/bin/env ruby", "ruby"),
            ("#!/usr/bin/env lua", "lua"),
            ("#!/usr/bin/env php", "php"),
            ("#!/usr/bin/swift", "swift"),
            ("#!/usr/bin/make -f", "make"),
            ("#!/usr/bin/env PYTHONPATH=. python3", "python"),
            ("\u{feff}#!/bin/sh", "shell"),
        ];
        for (line, expected) in cases {
            assert_eq!(id_for_line("script", line), Some(expected), "{line}");
        }
    }

    #[test]
    fn shebang_edge_cases() {
        assert_eq!(id_for_line("script", ""), None);
        assert_eq!(id_for_line("script", "#!"), None);
        assert_eq!(id_for_line("script", "#!/usr/bin/env"), None);
        assert_eq!(id_for_line("script", "#!/usr/bin/env -S"), None);
        assert_eq!(id_for_line("script", "# not a shebang"), None);
        assert_eq!(
            id_for_line("script", "#!/usr/bin/unknown-interpreter"),
            None
        );
        // The extension is more specific than the shebang.
        assert_eq!(id_for_line("tool.py", "#!/bin/sh"), Some("python"));
        assert_eq!(
            shebang_interpreter("#!/usr/bin/python3.12").as_deref(),
            Some("python")
        );
        assert_eq!(shebang_interpreter("#!/opt/bin/7").as_deref(), Some("7"));
    }

    #[test]
    fn sees_through_templates_and_chezmoi_names() {
        let cases = [
            ("AGENTS.md.tmpl", "markdown"),
            ("config.toml.tmpl", "toml"),
            ("dot_zshrc", "shell"),
            ("dot_zshrc.tmpl", "shell"),
            ("private_dot_bashrc", "shell"),
            ("executable_backup.sh.tmpl", "shell"),
            ("private_readonly_dot_gitignore", "gitignore"),
            ("run_once_before_install.sh.tmpl", "shell"),
            ("dot_config/nvim/init.lua.tmpl", "lua"),
        ];
        for (path, expected) in cases {
            assert_eq!(id_for(path), Some(expected), "{path}");
        }
        // A real extension wins over the chezmoi reading.
        assert_eq!(id_for("run_tests.py"), Some("python"));
        assert_eq!(id_for("dot_product.c"), Some("c"));
        assert_eq!(id_for("dot_unknown"), None);
        assert_eq!(id_for("dot_"), None);
    }

    #[test]
    fn unknown_files_are_plain_text() {
        let unknown = detect(Path::new("mystery.bin"), None);
        assert_eq!(unknown, plain_text());
        assert_eq!(unknown.id, "plain_text");
        assert!(!unknown.has_grammar());
        assert!(unknown.soft_wrap);
        assert!(unknown.capture_names().is_empty());
        assert_eq!(detect(Path::new("main.rs"), None).id, "rust");
        assert_eq!(detect(Path::new("tool"), Some("#!/bin/sh")).id, "shell");
    }

    #[test]
    fn looks_languages_up_by_id_and_by_name() {
        assert_eq!(language_by_id("rust").map(|l| l.name), Some("Rust"));
        assert_eq!(language_by_id("Rust"), None);
        assert_eq!(language_by_id("nope"), None);
        let cases = [
            ("rust", "rust"),
            ("rs", "rust"),
            ("RUST", "rust"),
            ("js", "javascript"),
            ("jsx", "javascript"),
            ("ts", "typescript"),
            ("py", "python"),
            ("sh", "shell"),
            ("bash", "shell"),
            ("zsh", "shell"),
            ("console", "shell"),
            ("Shell Script", "shell"),
            ("c++", "cpp"),
            ("C#", "csharp"),
            ("yml", "yaml"),
            ("md", "markdown"),
            ("golang", "go"),
            ("docker", "dockerfile"),
            ("makefile", "make"),
            ("patch", "diff"),
            ("tf", "terraform"),
            ("text", "plain_text"),
            (" rust ", "rust"),
        ];
        for (name, expected) in cases {
            assert_eq!(
                language_by_name(name).map(|l| l.id),
                Some(expected),
                "{name}"
            );
        }
        assert_eq!(language_by_name(""), None);
        assert_eq!(language_by_name("klingon"), None);
    }

    #[test]
    fn the_registry_is_consistent() {
        let all = languages();
        assert_eq!(all[0].id, "plain_text");
        for (i, language) in all.iter().enumerate() {
            assert!(!language.id.is_empty() && !language.name.is_empty());
            assert!(
                language
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{} is not a plain identifier",
                language.id
            );
            assert!(language.tab_size >= 1, "{}", language.id);
            for other in &all[i + 1..] {
                assert_ne!(language.id, other.id);
                assert_ne!(language.name, other.name);
                for extension in language.extensions {
                    assert!(
                        !other.extensions.contains(extension),
                        "{extension} is claimed twice"
                    );
                }
                for name in language.file_names {
                    assert!(!other.file_names.contains(name), "{name} is claimed twice");
                }
                for interpreter in language.interpreters {
                    assert!(
                        !other.interpreters.contains(interpreter),
                        "{interpreter} is claimed twice"
                    );
                }
                for alias in language.aliases {
                    assert!(!other.aliases.contains(alias), "{alias} is claimed twice");
                    assert_ne!(*alias, other.id, "{alias} is both an alias and an id");
                }
            }
            for extension in language.extensions {
                assert_eq!(*extension, extension.to_ascii_lowercase(), "{extension}");
                assert!(!extension.starts_with('.'), "{extension}");
            }
            for pair in language.autoclose {
                let is_quote = pair.open == pair.close;
                assert!(
                    is_quote || language.brackets.contains(pair),
                    "{} {pair:?}",
                    language.id
                );
            }
            if let Some(comment) = language.line_comment {
                assert!(comment.ends_with(' '), "{}", language.id);
            }
        }
    }

    #[test]
    fn editing_conventions_follow_zed() {
        let get = |id: &str| language_by_id(id).unwrap_or_else(|| panic!("no {id}"));
        // From the `languages` section of Zed's default settings.
        assert!(get("go").hard_tabs);
        assert_eq!(get("dart").tab_size, 2);
        for id in ["markdown", "plain_text", "git_commit"] {
            assert!(get(id).soft_wrap, "{id}");
        }
        // Everything else is four spaces, no wrapping.
        for id in [
            "rust", "python", "c", "cpp", "swift", "shell", "toml", "html", "css",
        ] {
            let language = get(id);
            assert_eq!(
                (language.tab_size, language.hard_tabs, language.soft_wrap),
                (4, false, false),
                "{id}"
            );
        }
        // A recipe line has to start with a tab, and gofmt writes go.mod with tabs.
        assert!(get("make").hard_tabs);
        assert!(get("gomod").hard_tabs);

        assert_eq!(get("rust").line_comment, Some("// "));
        assert_eq!(get("python").line_comment, Some("# "));
        assert_eq!(get("lua").line_comment, Some("-- "));
        assert_eq!(get("sql").line_comment, Some("-- "));
        assert_eq!(get("rust").block_comment, Some(("/* ", " */")));
        assert_eq!(get("html").block_comment, Some(("<!-- ", " -->")));
        assert_eq!(get("python").block_comment, None);

        let closes = |id: &str, open: char| get(id).autoclose.iter().any(|pair| pair.open == open);
        assert!(closes("rust", '"') && closes("rust", '{'));
        assert!(!closes("rust", '\''), "a lifetime is not a string");
        assert!(closes("python", '\'') && closes("javascript", '`'));
        assert!(!closes("markdown", '\'') && !closes("plain_text", '"'));

        assert!(get("python").indent_after.contains(':'));
        assert!(get("rust").indent_after.contains('{'));
        assert!(get("javascript").is_word_char('$'));
        assert!(get("css").is_word_char('-'));
        assert!(!get("rust").is_word_char('-'));
        assert!(get("rust").is_word_char('_') && get("rust").is_word_char('é'));
    }
}
