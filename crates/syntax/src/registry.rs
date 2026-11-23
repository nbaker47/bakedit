//! The language table.
//!
//! Indent sizes follow Zed: the `languages` section of its default settings gives Go hard
//! tabs, Dart two columns and soft wrap to Markdown, plain text and commit messages;
//! JavaScript, TypeScript, TSX, JSON and YAML are two columns in Zed as well (set in each
//! language's own configuration rather than in the settings file); everything else is four
//! spaces. Makefiles and `go.mod` use hard tabs because their tools require or write them.

use crate::grammar::{self, GrammarSpec};
use crate::language::{language_by_name, Language, Pair};

const ROUND: Pair = Pair::new('(', ')');
const SQUARE: Pair = Pair::new('[', ']');
const CURLY: Pair = Pair::new('{', '}');
const ANGLE: Pair = Pair::new('<', '>');
const DOUBLE: Pair = Pair::new('"', '"');
const SINGLE: Pair = Pair::new('\'', '\'');
const BACKTICK: Pair = Pair::new('`', '`');

/// The brackets of nearly every language.
const BRACKETS: &[Pair] = &[ROUND, SQUARE, CURLY];
/// Brackets, for prose: an apostrophe or a quotation mark is not half of a pair.
const CLOSE_BRACKETS: &[Pair] = &[ROUND, SQUARE, CURLY];
/// Brackets and double quotes.
const CLOSE_DOUBLE: &[Pair] = &[ROUND, SQUARE, CURLY, DOUBLE];
/// Brackets and both quotes.
const CLOSE_QUOTES: &[Pair] = &[ROUND, SQUARE, CURLY, DOUBLE, SINGLE];
/// Brackets, both quotes and the backtick.
const CLOSE_ALL: &[Pair] = &[ROUND, SQUARE, CURLY, DOUBLE, SINGLE, BACKTICK];

const C_BLOCK: Option<(&str, &str)> = Some(("/* ", " */"));
const HTML_BLOCK: Option<(&str, &str)> = Some(("<!-- ", " -->"));

/// The values most languages share; each entry below overrides what differs.
const BASE: Language = Language {
    id: "",
    name: "",
    extensions: &[],
    file_names: &[],
    interpreters: &[],
    aliases: &[],
    line_comment: None,
    block_comment: None,
    brackets: BRACKETS,
    autoclose: CLOSE_QUOTES,
    tab_size: 4,
    hard_tabs: false,
    soft_wrap: false,
    indent_after: "{[(",
    word_chars: "",
    grammar: None,
};

/// Every language. Plain text is first; the rest are in no particular order.
pub(crate) static LANGUAGES: &[Language] = &[
    Language {
        id: "plain_text",
        name: "Plain Text",
        extensions: &["txt", "text", "log"],
        file_names: &[
            "LICENSE",
            "LICENCE",
            "COPYING",
            "AUTHORS",
            "NOTICE",
            "CODEOWNERS",
            "go.sum",
        ],
        aliases: &["text", "plain", "plaintext"],
        autoclose: CLOSE_BRACKETS,
        soft_wrap: true,
        indent_after: "",
        ..BASE
    },
    Language {
        id: "rust",
        name: "Rust",
        extensions: &["rs"],
        aliases: &["rs"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        autoclose: CLOSE_DOUBLE,
        grammar: Some(&grammar::RUST),
        ..BASE
    },
    Language {
        id: "python",
        name: "Python",
        extensions: &["py", "pyi", "pyw"],
        interpreters: &["python", "pypy", "uv"],
        aliases: &["py", "python3"],
        line_comment: Some("# "),
        indent_after: "{[(:",
        grammar: Some(&grammar::PYTHON),
        ..BASE
    },
    Language {
        id: "javascript",
        name: "JavaScript",
        extensions: &["js", "mjs", "cjs", "jsx"],
        interpreters: &["node", "nodejs", "deno", "bun"],
        aliases: &["js", "jsx", "node", "ecmascript"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        autoclose: CLOSE_ALL,
        tab_size: 2,
        word_chars: "$#",
        grammar: Some(&grammar::JAVASCRIPT),
        ..BASE
    },
    Language {
        id: "typescript",
        name: "TypeScript",
        extensions: &["ts", "mts", "cts"],
        interpreters: &["ts-node", "tsx"],
        aliases: &["ts"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        autoclose: CLOSE_ALL,
        tab_size: 2,
        word_chars: "$#",
        grammar: Some(&grammar::TYPESCRIPT),
        ..BASE
    },
    Language {
        id: "tsx",
        name: "TSX",
        extensions: &["tsx"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        autoclose: CLOSE_ALL,
        tab_size: 2,
        word_chars: "$#",
        grammar: Some(&grammar::TSX),
        ..BASE
    },
    Language {
        id: "json",
        name: "JSON",
        extensions: &[
            "json",
            "jsonc",
            "json5",
            "jsonl",
            "ndjson",
            "geojson",
            "webmanifest",
            "code-workspace",
        ],
        file_names: &[
            ".babelrc",
            ".eslintrc",
            ".prettierrc",
            ".swcrc",
            ".jshintrc",
            ".hintrc",
            ".watchmanconfig",
            "composer.lock",
            "flake.lock",
            "Pipfile.lock",
            "Package.resolved",
            "bun.lock",
            "deno.lock",
        ],
        aliases: &["jsonc", "json5"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        autoclose: CLOSE_DOUBLE,
        tab_size: 2,
        indent_after: "{[",
        grammar: Some(&grammar::JSON),
        ..BASE
    },
    Language {
        id: "toml",
        name: "TOML",
        extensions: &["toml"],
        file_names: &[
            "Cargo.lock",
            "poetry.lock",
            "uv.lock",
            "pdm.lock",
            "Gopkg.lock",
            "Pipfile",
        ],
        line_comment: Some("# "),
        indent_after: "{[",
        grammar: Some(&grammar::TOML),
        ..BASE
    },
    Language {
        id: "yaml",
        name: "YAML",
        extensions: &["yaml", "yml", "cff"],
        file_names: &[".clang-format", ".clang-tidy", ".yamllint"],
        aliases: &["yml"],
        line_comment: Some("# "),
        tab_size: 2,
        indent_after: "{[:",
        grammar: Some(&grammar::YAML),
        ..BASE
    },
    Language {
        id: "markdown",
        name: "Markdown",
        extensions: &["md", "markdown", "mdown", "mkd", "mdx"],
        aliases: &["md"],
        block_comment: HTML_BLOCK,
        autoclose: CLOSE_BRACKETS,
        soft_wrap: true,
        indent_after: "",
        grammar: Some(&grammar::MARKDOWN),
        ..BASE
    },
    Language {
        id: "shell",
        name: "Shell Script",
        extensions: &["sh", "bash", "zsh", "ksh", "command", "envrc"],
        file_names: &[
            ".bashrc",
            ".bash_profile",
            ".bash_aliases",
            ".bash_logout",
            ".bash_login",
            ".profile",
            ".zshrc",
            ".zshenv",
            ".zprofile",
            ".zlogin",
            ".zlogout",
            ".zsh_aliases",
            ".envrc",
            ".env",
            ".env.*",
            ".xinitrc",
            ".xprofile",
            "PKGBUILD",
            "APKBUILD",
        ],
        interpreters: &["sh", "bash", "zsh", "dash", "ksh", "ash"],
        aliases: &[
            "sh",
            "bash",
            "zsh",
            "console",
            "shell-session",
            "shellscript",
        ],
        line_comment: Some("# "),
        autoclose: CLOSE_ALL,
        grammar: Some(&grammar::BASH),
        ..BASE
    },
    Language {
        id: "go",
        name: "Go",
        extensions: &["go"],
        aliases: &["golang"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        autoclose: CLOSE_ALL,
        hard_tabs: true,
        grammar: Some(&grammar::GO),
        ..BASE
    },
    Language {
        id: "c",
        name: "C",
        extensions: &["c", "h"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        grammar: Some(&grammar::C),
        ..BASE
    },
    Language {
        id: "cpp",
        name: "C++",
        extensions: &[
            "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "ipp", "inl", "tpp", "cu", "cuh",
        ],
        aliases: &["c++", "cxx"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        grammar: Some(&grammar::CPP),
        ..BASE
    },
    Language {
        id: "html",
        name: "HTML",
        extensions: &["html", "htm", "shtml", "xhtml"],
        block_comment: HTML_BLOCK,
        brackets: &[ROUND, SQUARE, CURLY, ANGLE],
        word_chars: "-",
        grammar: Some(&grammar::HTML),
        ..BASE
    },
    Language {
        id: "css",
        name: "CSS",
        extensions: &["css"],
        block_comment: C_BLOCK,
        word_chars: "-",
        grammar: Some(&grammar::CSS),
        ..BASE
    },
    Language {
        id: "swift",
        name: "Swift",
        extensions: &["swift"],
        interpreters: &["swift"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        autoclose: CLOSE_DOUBLE,
        grammar: Some(&grammar::SWIFT),
        ..BASE
    },
    Language {
        id: "java",
        name: "Java",
        extensions: &["java"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        grammar: Some(&grammar::JAVA),
        ..BASE
    },
    Language {
        id: "lua",
        name: "Lua",
        extensions: &["lua"],
        interpreters: &["lua", "luajit"],
        line_comment: Some("-- "),
        block_comment: Some(("--[[ ", " ]]")),
        grammar: Some(&grammar::LUA),
        ..BASE
    },
    Language {
        id: "dockerfile",
        name: "Dockerfile",
        extensions: &["dockerfile", "containerfile"],
        file_names: &[
            "Dockerfile",
            "Dockerfile.*",
            "Containerfile",
            "Containerfile.*",
        ],
        aliases: &["docker", "containerfile"],
        line_comment: Some("# "),
        grammar: Some(&grammar::DOCKERFILE),
        ..BASE
    },
    Language {
        id: "sql",
        name: "SQL",
        extensions: &["sql", "psql", "pgsql", "mysql", "ddl", "dml"],
        aliases: &["postgres", "postgresql", "sqlite"],
        line_comment: Some("-- "),
        block_comment: C_BLOCK,
        indent_after: "(",
        grammar: Some(&grammar::SQL),
        ..BASE
    },
    Language {
        id: "ruby",
        name: "Ruby",
        extensions: &["rb", "rake", "gemspec", "ru", "podspec"],
        file_names: &[
            "Gemfile",
            "Rakefile",
            "Brewfile",
            "Podfile",
            "Fastfile",
            "Appfile",
            "Vagrantfile",
            "Guardfile",
        ],
        interpreters: &["ruby"],
        aliases: &["rb"],
        line_comment: Some("# "),
        word_chars: "?!",
        grammar: Some(&grammar::RUBY),
        ..BASE
    },
    Language {
        id: "kotlin",
        name: "Kotlin",
        extensions: &["kt", "kts"],
        interpreters: &["kotlin"],
        aliases: &["kt", "kts"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        grammar: Some(&grammar::KOTLIN),
        ..BASE
    },
    Language {
        id: "diff",
        name: "Diff",
        extensions: &["diff", "patch", "rej"],
        aliases: &["patch", "udiff"],
        autoclose: &[],
        indent_after: "",
        grammar: Some(&grammar::DIFF),
        ..BASE
    },
    Language {
        id: "make",
        name: "Makefile",
        extensions: &["mk", "mak", "make"],
        file_names: &["Makefile", "makefile", "GNUmakefile", "BSDmakefile"],
        interpreters: &["make", "gmake"],
        aliases: &["makefile", "mk"],
        line_comment: Some("# "),
        hard_tabs: true,
        indent_after: ":",
        grammar: Some(&grammar::MAKE),
        ..BASE
    },
    Language {
        id: "nix",
        name: "Nix",
        extensions: &["nix"],
        line_comment: Some("# "),
        block_comment: C_BLOCK,
        autoclose: CLOSE_DOUBLE,
        word_chars: "-'",
        grammar: Some(&grammar::NIX),
        ..BASE
    },
    Language {
        id: "zig",
        name: "Zig",
        extensions: &["zig", "zon"],
        line_comment: Some("// "),
        grammar: Some(&grammar::ZIG),
        ..BASE
    },
    Language {
        id: "php",
        name: "PHP",
        extensions: &["php", "phtml", "php3", "php4", "php5", "phps"],
        interpreters: &["php"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        word_chars: "$",
        grammar: Some(&grammar::PHP),
        ..BASE
    },
    Language {
        id: "csharp",
        name: "CSharp",
        extensions: &["cs", "csx"],
        aliases: &["c#", "cs", "c_sharp", "dotnet"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        grammar: Some(&grammar::CSHARP),
        ..BASE
    },
    Language {
        id: "dart",
        name: "Dart",
        extensions: &["dart"],
        interpreters: &["dart"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        tab_size: 2,
        word_chars: "$",
        grammar: Some(&grammar::DART),
        ..BASE
    },
    Language {
        id: "gomod",
        name: "Go Mod",
        file_names: &["go.mod", "go.work"],
        aliases: &["go.mod"],
        line_comment: Some("// "),
        hard_tabs: true,
        indent_after: "(",
        grammar: Some(&grammar::GOMOD),
        ..BASE
    },
    Language {
        id: "git_commit",
        name: "Git Commit",
        file_names: &[
            "COMMIT_EDITMSG",
            "MERGE_MSG",
            "TAG_EDITMSG",
            "NOTES_EDITMSG",
            "EDIT_DESCRIPTION",
        ],
        aliases: &["gitcommit", "commit"],
        line_comment: Some("# "),
        autoclose: CLOSE_BRACKETS,
        soft_wrap: true,
        indent_after: "",
        grammar: Some(&grammar::GIT_COMMIT),
        ..BASE
    },
    // The languages below were wanted with highlighting, but have no grammar crate that both
    // builds against this tree-sitter and ships a usable highlights query. They are plain
    // text with the right conventions.
    Language {
        id: "terraform",
        name: "Terraform",
        extensions: &["tf", "tfvars"],
        aliases: &["tf"],
        line_comment: Some("# "),
        block_comment: C_BLOCK,
        autoclose: CLOSE_DOUBLE,
        tab_size: 2,
        ..BASE
    },
    Language {
        id: "hcl",
        name: "HCL",
        extensions: &["hcl", "nomad"],
        line_comment: Some("# "),
        block_comment: C_BLOCK,
        autoclose: CLOSE_DOUBLE,
        tab_size: 2,
        ..BASE
    },
    Language {
        id: "gdscript",
        name: "GDScript",
        extensions: &["gd"],
        aliases: &["gd", "godot"],
        line_comment: Some("# "),
        hard_tabs: true,
        indent_after: "{[(:",
        ..BASE
    },
    Language {
        id: "vue",
        name: "Vue.js",
        extensions: &["vue"],
        block_comment: HTML_BLOCK,
        brackets: &[ROUND, SQUARE, CURLY, ANGLE],
        autoclose: CLOSE_ALL,
        word_chars: "-",
        ..BASE
    },
    Language {
        id: "svelte",
        name: "Svelte",
        extensions: &["svelte"],
        block_comment: HTML_BLOCK,
        brackets: &[ROUND, SQUARE, CURLY, ANGLE],
        autoclose: CLOSE_ALL,
        word_chars: "-",
        ..BASE
    },
    Language {
        id: "just",
        name: "Just",
        extensions: &["just"],
        file_names: &["Justfile", "justfile", ".justfile"],
        aliases: &["justfile"],
        line_comment: Some("# "),
        indent_after: ":",
        ..BASE
    },
    Language {
        id: "gitignore",
        name: "Git Ignore",
        extensions: &["gitignore"],
        file_names: &[
            ".gitignore",
            ".dockerignore",
            ".npmignore",
            ".prettierignore",
            ".eslintignore",
            ".ignore",
            ".rgignore",
            ".fdignore",
            ".stignore",
            ".vercelignore",
            ".helmignore",
        ],
        aliases: &["ignore"],
        line_comment: Some("# "),
        autoclose: &[],
        indent_after: "",
        ..BASE
    },
    // Common formats nobody asked to have highlighted. Knowing them still gets comment
    // toggling and bracket matching right, and keeps them from being soft-wrapped as prose.
    Language {
        id: "xml",
        name: "XML",
        extensions: &[
            "xml",
            "plist",
            "svg",
            "xsd",
            "xsl",
            "xslt",
            "xib",
            "storyboard",
            "entitlements",
            "csproj",
            "props",
            "targets",
        ],
        block_comment: HTML_BLOCK,
        brackets: &[ROUND, SQUARE, CURLY, ANGLE],
        word_chars: "-",
        ..BASE
    },
    Language {
        id: "ini",
        name: "INI",
        extensions: &[
            "ini",
            "cfg",
            "conf",
            "properties",
            "desktop",
            "service",
            "editorconfig",
        ],
        file_names: &[
            ".editorconfig",
            ".gitconfig",
            ".gitmodules",
            ".npmrc",
            ".gitattributes",
        ],
        aliases: &["dosini", "gitconfig"],
        line_comment: Some("# "),
        indent_after: "",
        ..BASE
    },
    Language {
        id: "cmake",
        name: "CMake",
        extensions: &["cmake"],
        file_names: &["CMakeLists.txt"],
        line_comment: Some("# "),
        autoclose: CLOSE_DOUBLE,
        ..BASE
    },
    Language {
        id: "proto",
        name: "Proto",
        extensions: &["proto"],
        aliases: &["protobuf"],
        line_comment: Some("// "),
        block_comment: C_BLOCK,
        tab_size: 2,
        ..BASE
    },
    Language {
        id: "graphql",
        name: "GraphQL",
        extensions: &["graphql", "graphqls", "gql"],
        line_comment: Some("# "),
        autoclose: CLOSE_DOUBLE,
        tab_size: 2,
        ..BASE
    },
];

/// Grammars that exist only to be injected into another language.
const INJECTED_ONLY: &[&GrammarSpec] = &[&grammar::MARKDOWN_INLINE, &grammar::REGEX];

/// The grammar an injection or a fenced code block names, or `None` when this crate has no
/// grammar by that name.
pub(crate) fn grammar_by_name(name: &str) -> Option<&'static GrammarSpec> {
    let name = name.trim();
    INJECTED_ONLY
        .iter()
        .find(|spec| spec.name.eq_ignore_ascii_case(name))
        .copied()
        .or_else(|| language_by_name(name)?.grammar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::load;

    #[test]
    fn every_grammar_loads() {
        for spec in grammar::ALL {
            if let Err(error) = load(spec) {
                panic!("{error}");
            }
        }
    }

    #[test]
    fn every_grammar_is_reachable() {
        for spec in grammar::ALL {
            let by_language = LANGUAGES
                .iter()
                .any(|l| l.grammar.is_some_and(|g| std::ptr::eq(g, *spec)));
            let injected_only = INJECTED_ONLY.iter().any(|g| std::ptr::eq(*g, *spec));
            assert!(
                by_language != injected_only,
                "{} must be listed exactly once",
                spec.name
            );
            assert!(
                grammar_by_name(spec.name).is_some_and(|g| std::ptr::eq(g, *spec)),
                "{} is not found by its own name",
                spec.name
            );
        }
    }

    #[test]
    fn resolves_the_names_injections_use() {
        let name_of = |name: &str| grammar_by_name(name).map(|spec| spec.name);
        assert_eq!(name_of("markdown_inline"), Some("markdown_inline"));
        assert_eq!(name_of("regex"), Some("regex"));
        assert_eq!(name_of("javascript"), Some("javascript"));
        assert_eq!(name_of("js"), Some("javascript"));
        assert_eq!(name_of("ts"), Some("typescript"));
        assert_eq!(name_of("typescript"), Some("typescript"));
        assert_eq!(name_of("css"), Some("css"));
        assert_eq!(name_of("html"), Some("html"));
        assert_eq!(name_of("yaml"), Some("yaml"));
        assert_eq!(name_of("toml"), Some("toml"));
        assert_eq!(name_of("bash"), Some("bash"));
        assert_eq!(name_of("sh"), Some("bash"));
        assert_eq!(name_of("SQL"), Some("sql"));
        assert_eq!(name_of("c"), Some("c"));
        assert_eq!(name_of("diff"), Some("diff"));
        assert_eq!(name_of("rust"), Some("rust"));
        assert_eq!(name_of("py"), Some("python"));
        // Languages this crate knows but cannot highlight, and ones it does not know.
        assert_eq!(name_of("terraform"), None);
        assert_eq!(name_of("plain"), None);
        assert_eq!(name_of("jsdoc"), None);
        assert_eq!(name_of("comment"), None);
        assert_eq!(name_of("latex"), None);
        assert_eq!(name_of(""), None);
    }
}
