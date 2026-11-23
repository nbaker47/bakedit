//! File and folder icons: Nerd Font glyphs in the Catppuccin Frappé colours.
//!
//! The mapping mirrors the "Catppuccin Frappé" Zed icon theme: each file type gets the
//! colour that theme's icon is drawn in, and a Nerd Font glyph standing in for its
//! picture. Every glyph is a single private-use code point, one terminal cell wide.
//!
//! A file is looked up in three steps, the first hit winning:
//!
//! 1. its whole name (`Cargo.toml`, `Makefile`, `.gitignore`), ignoring case;
//! 2. a family prefix (`.env*`, `Dockerfile.*`, `LICENSE*`, `README*`, ...);
//! 3. its last extension (`rs`, `tsx`, `png`).
//!
//! Directories all share the folder glyph (closed or open); the well-known names of the
//! Catppuccin theme (`src`, `tests`, `docs`, ...) get that theme's accent colour.

use std::collections::HashMap;
use std::sync::OnceLock;

use bakedit_core::Rgb;

/// The Catppuccin Frappé colours the icons are drawn in.
pub mod frappe {
    use bakedit_core::Rgb;

    pub const ROSEWATER: Rgb = Rgb::new(0xf2, 0xd5, 0xcf);
    pub const FLAMINGO: Rgb = Rgb::new(0xee, 0xbe, 0xbe);
    pub const PINK: Rgb = Rgb::new(0xf4, 0xb8, 0xe4);
    pub const MAUVE: Rgb = Rgb::new(0xca, 0x9e, 0xe6);
    pub const RED: Rgb = Rgb::new(0xe7, 0x82, 0x84);
    pub const MAROON: Rgb = Rgb::new(0xea, 0x99, 0x9c);
    pub const PEACH: Rgb = Rgb::new(0xef, 0x9f, 0x76);
    pub const YELLOW: Rgb = Rgb::new(0xe5, 0xc8, 0x90);
    pub const GREEN: Rgb = Rgb::new(0xa6, 0xd1, 0x89);
    pub const TEAL: Rgb = Rgb::new(0x81, 0xc8, 0xbe);
    pub const SKY: Rgb = Rgb::new(0x99, 0xd1, 0xdb);
    pub const SAPPHIRE: Rgb = Rgb::new(0x85, 0xc1, 0xdc);
    pub const BLUE: Rgb = Rgb::new(0x8c, 0xaa, 0xee);
    pub const LAVENDER: Rgb = Rgb::new(0xba, 0xbb, 0xf1);
    pub const TEXT: Rgb = Rgb::new(0xc6, 0xd0, 0xf5);
    pub const OVERLAY1: Rgb = Rgb::new(0x83, 0x8b, 0xa7);
}

use frappe::*;

/// One icon: the glyph to print (exactly one terminal cell wide) and its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icon {
    pub glyph: &'static str,
    pub color: Rgb,
}

/// Nerd Font glyphs, named after their entry in the Nerd Fonts cheat sheet.
mod glyph {
    pub const FOLDER: &str = "\u{f114}"; // fa-folder_o
    pub const FOLDER_OPEN: &str = "\u{f115}"; // fa-folder_open_o
    pub const FILE: &str = "\u{f016}"; // fa-file_o

    pub const ARCHIVE: &str = "\u{f410}"; // oct-file_zip
    pub const ASM: &str = "\u{e637}"; // seti-asm
    pub const ASTRO: &str = "\u{e6b3}"; // custom-astro
    pub const AUDIO: &str = "\u{e638}"; // seti-audio
    pub const BAZEL: &str = "\u{e63a}"; // seti-bazel
    pub const BINARY: &str = "\u{eae8}"; // cod-file_binary
    pub const BOOK: &str = "\u{f02d}"; // fa-book
    pub const C: &str = "\u{e61e}"; // custom-c
    pub const CERTIFICATE: &str = "\u{f0a3}"; // fa-certificate
    pub const CLOJURE: &str = "\u{e642}"; // seti-clojure
    pub const CMAKE: &str = "\u{e794}"; // dev-cmake
    pub const CONFIG: &str = "\u{e615}"; // seti-config
    pub const CPP: &str = "\u{e61d}"; // custom-cpp
    pub const CRYSTAL: &str = "\u{e62f}"; // seti-crystal
    pub const CSHARP: &str = "\u{e648}"; // seti-c_sharp
    pub const CSS: &str = "\u{e749}"; // dev-css3
    pub const CSV: &str = "\u{e64a}"; // seti-csv
    pub const CUBE: &str = "\u{f1b2}"; // fa-cube
    pub const D: &str = "\u{e651}"; // seti-d
    pub const DART: &str = "\u{e64c}"; // seti-dart
    pub const DATABASE: &str = "\u{e706}"; // dev-database
    pub const DIFF: &str = "\u{e728}"; // dev-git_compare
    pub const DOCKER: &str = "\u{f308}"; // linux-docker
    pub const EDITORCONFIG: &str = "\u{e652}"; // seti-editorconfig
    pub const EJS: &str = "\u{e618}"; // seti-ejs
    pub const ELIXIR: &str = "\u{e62d}"; // seti-elixir
    pub const ELM: &str = "\u{e62c}"; // seti-elm
    pub const ERLANG: &str = "\u{e7b1}"; // dev-erlang
    pub const ESLINT: &str = "\u{e655}"; // seti-eslint
    pub const EXCEL: &str = "\u{e6a6}"; // seti-xls
    pub const FAVICON: &str = "\u{e623}"; // seti-favicon
    pub const FONT: &str = "\u{e659}"; // seti-font
    pub const FSHARP: &str = "\u{e65a}"; // seti-f_sharp
    pub const GIT: &str = "\u{e702}"; // dev-git
    pub const GNU: &str = "\u{e779}"; // dev-gnu
    pub const GO: &str = "\u{e627}"; // seti-go
    pub const GODOT: &str = "\u{e65f}"; // seti-godot
    pub const GRADLE: &str = "\u{e660}"; // seti-gradle
    pub const GRAPHQL: &str = "\u{e662}"; // seti-graphql
    pub const HAML: &str = "\u{e664}"; // seti-haml
    pub const HASKELL: &str = "\u{e61f}"; // seti-haskell
    pub const HISTORY: &str = "\u{f1da}"; // fa-history
    pub const HTML: &str = "\u{e736}"; // dev-html5
    pub const IMAGE: &str = "\u{e60d}"; // seti-image
    pub const JAVA: &str = "\u{e738}"; // dev-java
    pub const JAVASCRIPT: &str = "\u{e60c}"; // seti-javascript
    pub const JINJA: &str = "\u{e66f}"; // seti-jinja
    pub const JSON: &str = "\u{e60b}"; // seti-json
    pub const JULIA: &str = "\u{e624}"; // seti-julia
    pub const KEY: &str = "\u{f084}"; // fa-key
    pub const KOTLIN: &str = "\u{e634}"; // seti-kotlin
    pub const LICENSE: &str = "\u{e60a}"; // seti-license
    pub const LIQUID: &str = "\u{e670}"; // seti-liquid
    pub const LOCK: &str = "\u{e672}"; // seti-lock
    pub const LOG: &str = "\u{f4ed}"; // oct-log
    pub const LUA: &str = "\u{e620}"; // seti-lua
    pub const MARKDOWN: &str = "\u{f48a}"; // oct-markdown
    pub const MUSTACHE: &str = "\u{e60f}"; // seti-mustache
    pub const NIM: &str = "\u{e677}"; // seti-nim
    pub const NIX: &str = "\u{f313}"; // linux-nixos
    pub const NODE: &str = "\u{e718}"; // dev-nodejs_small
    pub const NOTEBOOK: &str = "\u{e678}"; // seti-notebook
    pub const NPM: &str = "\u{e71e}"; // dev-npm
    pub const OCAML: &str = "\u{e67a}"; // seti-ocaml
    pub const PDF: &str = "\u{e67d}"; // seti-pdf
    pub const PERL: &str = "\u{e67e}"; // seti-perl
    pub const PHP: &str = "\u{e608}"; // seti-php
    pub const POWERPOINT: &str = "\u{f1c4}"; // fa-file_powerpoint_o
    pub const POWERSHELL: &str = "\u{e683}"; // seti-powershell
    pub const PRETTIER: &str = "\u{e6b4}"; // custom-prettier
    pub const PRISMA: &str = "\u{e684}"; // seti-prisma
    pub const PUG: &str = "\u{e686}"; // seti-pug
    pub const PYTHON: &str = "\u{e606}"; // seti-python
    pub const R: &str = "\u{e68a}"; // seti-r
    pub const REACT: &str = "\u{e7ba}"; // dev-react
    pub const RUBY: &str = "\u{e739}"; // dev-ruby
    pub const RUST: &str = "\u{e7a8}"; // dev-rust
    pub const SASS: &str = "\u{e603}"; // seti-sass
    pub const SCALA: &str = "\u{e68e}"; // seti-scala
    pub const SOLIDITY: &str = "\u{e656}"; // seti-ethereum
    pub const STRUCTURE: &str = "\u{ea91}"; // cod-symbol_structure
    pub const SVELTE: &str = "\u{e697}"; // seti-svelte
    pub const SVG: &str = "\u{e698}"; // seti-svg
    pub const SWIFT: &str = "\u{e755}"; // dev-swift
    pub const TERMINAL: &str = "\u{e795}"; // dev-terminal
    pub const TERRAFORM: &str = "\u{e69a}"; // seti-terraform
    pub const TEX: &str = "\u{e69b}"; // seti-tex
    pub const TEXT: &str = "\u{f0f6}"; // fa-file_text_o
    pub const TODO: &str = "\u{e69c}"; // seti-todo
    pub const TOML: &str = "\u{e6b2}"; // custom-toml
    pub const TSCONFIG: &str = "\u{e69d}"; // seti-tsconfig
    pub const TWIG: &str = "\u{e61c}"; // seti-twig
    pub const TYPESCRIPT: &str = "\u{e628}"; // seti-typescript
    pub const VIDEO: &str = "\u{e69f}"; // seti-video
    pub const VIM: &str = "\u{e62b}"; // custom-vim
    pub const VUE: &str = "\u{e6a0}"; // seti-vue
    pub const WASM: &str = "\u{e6a1}"; // seti-wasm
    pub const WORD: &str = "\u{e6a5}"; // seti-word
    pub const WRENCH: &str = "\u{f0ad}"; // fa-wrench
    pub const XML: &str = "\u{e619}"; // seti-xml
    pub const YAML: &str = "\u{e6a8}"; // seti-yml
    pub const ZIG: &str = "\u{e6a9}"; // seti-zig
}

/// The glyphs [`ascii_icon_for`] uses: plain Unicode every terminal font has.
mod ascii {
    pub const FOLDER: &str = "▸";
    pub const FOLDER_OPEN: &str = "▾";
    pub const FILE: &str = "·";
}

/// A set of names that share one icon.
struct Group {
    names: &'static [&'static str],
    glyph: &'static str,
    color: Rgb,
}

const fn group(names: &'static [&'static str], glyph: &'static str, color: Rgb) -> Group {
    Group {
        names,
        glyph,
        color,
    }
}

/// The icon of a file nothing else matched.
const DEFAULT_FILE: Icon = Icon {
    glyph: glyph::FILE,
    color: TEXT,
};

const DOCKER: Icon = Icon {
    glyph: glyph::DOCKER,
    color: BLUE,
};
const DOCKER_COMPOSE: Icon = Icon {
    glyph: glyph::DOCKER,
    color: TEAL,
};
const ENV: Icon = Icon {
    glyph: glyph::KEY,
    color: YELLOW,
};
const GIT: Icon = Icon {
    glyph: glyph::GIT,
    color: PEACH,
};
const LICENSE: Icon = Icon {
    glyph: glyph::LICENSE,
    color: PEACH,
};
const README: Icon = Icon {
    glyph: glyph::BOOK,
    color: TEAL,
};
const CHANGELOG: Icon = Icon {
    glyph: glyph::HISTORY,
    color: TEAL,
};
const ESLINT: Icon = Icon {
    glyph: glyph::ESLINT,
    color: MAUVE,
};
const PRETTIER: Icon = Icon {
    glyph: glyph::PRETTIER,
    color: SAPPHIRE,
};
const TSCONFIG: Icon = Icon {
    glyph: glyph::TSCONFIG,
    color: BLUE,
};

/// Whole file names, lower case.
static FILE_NAMES: &[Group] = &[
    group(&["cargo.toml"], glyph::RUST, PEACH),
    group(&["cargo.lock"], glyph::LOCK, PEACH),
    group(&["makefile", "gnumakefile", "kbuild"], glyph::GNU, PEACH),
    group(&["justfile", ".justfile"], glyph::WRENCH, TEXT),
    group(&["dockerfile", "containerfile"], glyph::DOCKER, BLUE),
    group(
        &[".dockerignore", ".containerignore"],
        glyph::DOCKER,
        OVERLAY1,
    ),
    group(&["package.json"], glyph::NPM, GREEN),
    group(&["package-lock.json", "gemfile.lock"], glyph::LOCK, RED),
    group(&["yarn.lock"], glyph::LOCK, MAUVE),
    group(&["pnpm-lock.yaml"], glyph::LOCK, PEACH),
    group(&["bun.lock", "bun.lockb"], glyph::LOCK, ROSEWATER),
    group(&["uv.lock"], glyph::LOCK, PINK),
    group(&["poetry.lock"], glyph::LOCK, SKY),
    group(&["flake.lock"], glyph::LOCK, SAPPHIRE),
    group(&["deno.lock"], glyph::LOCK, TEXT),
    group(&["composer.lock"], glyph::LOCK, YELLOW),
    group(&[".npmrc"], glyph::NPM, RED),
    group(&[".npmignore"], glyph::NPM, OVERLAY1),
    group(&[".nvmrc", ".node-version"], glyph::NODE, GREEN),
    group(
        &["go.mod", "go.sum", "go.work", "go.work.sum"],
        glyph::GO,
        SAPPHIRE,
    ),
    group(
        &[
            "pyproject.toml",
            "requirements.txt",
            "requirements-dev.txt",
            "requirements-test.txt",
            ".python-version",
        ],
        glyph::PYTHON,
        YELLOW,
    ),
    group(&["gemfile", "rakefile", ".ruby-version"], glyph::RUBY, RED),
    group(&["cmakelists.txt"], glyph::CMAKE, GREEN),
    group(&["todo", "todo.md", "todos.md"], glyph::TODO, GREEN),
    group(&["favicon.ico"], glyph::FAVICON, YELLOW),
    group(&["codeowners", "owners"], glyph::BOOK, PEACH),
];

/// Last extensions (and the names of dotfiles without their dot), lower case.
static EXTENSIONS: &[Group] = &[
    // Languages.
    group(&["rs", "ron"], glyph::RUST, PEACH),
    group(&["py", "pyi", "pyw", "pyx"], glyph::PYTHON, BLUE),
    group(&["pyc", "pyo"], glyph::PYTHON, OVERLAY1),
    group(&["ipynb"], glyph::NOTEBOOK, PEACH),
    group(&["js", "mjs", "cjs"], glyph::JAVASCRIPT, YELLOW),
    group(&["jsx"], glyph::REACT, SKY),
    group(&["ts", "mts", "cts"], glyph::TYPESCRIPT, BLUE),
    group(&["tsx"], glyph::REACT, BLUE),
    group(
        &["go", "tmpl", "gotmpl", "gohtml", "templ"],
        glyph::GO,
        SAPPHIRE,
    ),
    group(&["c", "m"], glyph::C, BLUE),
    group(&["h"], glyph::C, MAUVE),
    group(&["cpp", "cc", "cxx", "c++", "mm"], glyph::CPP, BLUE),
    group(&["hpp", "hh", "hxx", "h++", "inl"], glyph::CPP, MAUVE),
    group(&["swift"], glyph::SWIFT, PEACH),
    group(&["kt", "kts"], glyph::KOTLIN, MAUVE),
    group(&["java", "jsp"], glyph::JAVA, RED),
    group(&["class"], glyph::JAVA, BLUE),
    group(&["jar", "war"], glyph::JAVA, OVERLAY1),
    group(&["rb", "erb", "rbs", "gemspec", "rake"], glyph::RUBY, RED),
    group(&["lua"], glyph::LUA, BLUE),
    group(&["luau"], glyph::LUA, SKY),
    group(&["php"], glyph::PHP, BLUE),
    group(&["cs", "csx"], glyph::CSHARP, BLUE),
    group(&["zig", "zon"], glyph::ZIG, YELLOW),
    group(&["dart"], glyph::DART, SAPPHIRE),
    group(&["gd"], glyph::GODOT, SKY),
    group(
        &[
            "tscn",
            "tres",
            "godot",
            "gdshader",
            "gdshaderinc",
            "gdextension",
        ],
        glyph::GODOT,
        GREEN,
    ),
    group(&["import", "uid"], glyph::GODOT, OVERLAY1),
    group(&["nix"], glyph::NIX, SAPPHIRE),
    group(
        &["tf", "tfvars", "tfstate", "tfbackend", "hcl"],
        glyph::TERRAFORM,
        MAUVE,
    ),
    group(&["ex", "exs", "heex"], glyph::ELIXIR, MAUVE),
    group(&["erl", "hrl"], glyph::ERLANG, RED),
    group(&["hs", "lhs"], glyph::HASKELL, MAUVE),
    group(&["scala", "sc", "sbt"], glyph::SCALA, RED),
    group(&["clj", "cljs", "cljc", "edn"], glyph::CLOJURE, GREEN),
    group(&["ml", "mli"], glyph::OCAML, PEACH),
    group(&["fs", "fsx", "fsi"], glyph::FSHARP, BLUE),
    group(&["r", "rmd"], glyph::R, BLUE),
    group(&["jl"], glyph::JULIA, GREEN),
    group(&["pl", "pm"], glyph::PERL, BLUE),
    group(&["elm"], glyph::ELM, BLUE),
    group(&["nim"], glyph::NIM, YELLOW),
    group(&["cr"], glyph::CRYSTAL, TEXT),
    group(&["d"], glyph::D, MAROON),
    group(&["sol"], glyph::SOLIDITY, MAUVE),
    group(&["asm", "s"], glyph::ASM, TEXT),
    group(&["wasm", "wat"], glyph::WASM, MAUVE),
    group(
        &["glsl", "wgsl", "hlsl", "vert", "frag", "shader", "metal"],
        glyph::CUBE,
        MAUVE,
    ),
    // Shells and scripts.
    group(
        &[
            "sh",
            "bash",
            "zsh",
            "fish",
            "ksh",
            "csh",
            "tcsh",
            "awk",
            "zshrc",
            "zshenv",
            "zprofile",
            "zlogin",
            "zlogout",
            "bashrc",
            "bash_profile",
            "bash_aliases",
            "bash_logout",
            "profile",
        ],
        glyph::TERMINAL,
        GREEN,
    ),
    group(&["bat", "cmd"], glyph::TERMINAL, BLUE),
    group(&["ps1", "psm1", "psd1"], glyph::POWERSHELL, BLUE),
    group(&["vim", "vimrc", "gvimrc"], glyph::VIM, GREEN),
    group(&["mk"], glyph::GNU, PEACH),
    group(&["just"], glyph::WRENCH, TEXT),
    group(&["cmake"], glyph::CMAKE, GREEN),
    group(&["gradle"], glyph::GRADLE, SAPPHIRE),
    group(&["bazel", "bzl"], glyph::BAZEL, GREEN),
    group(&["dockerfile", "containerfile"], glyph::DOCKER, BLUE),
    group(
        &["dockerignore", "containerignore"],
        glyph::DOCKER,
        OVERLAY1,
    ),
    // Web.
    group(&["html", "htm", "xhtml"], glyph::HTML, PEACH),
    group(&["css"], glyph::CSS, MAUVE),
    group(&["less"], glyph::CSS, BLUE),
    group(&["scss", "sass"], glyph::SASS, PINK),
    group(&["vue"], glyph::VUE, GREEN),
    group(&["svelte"], glyph::SVELTE, PEACH),
    group(&["astro"], glyph::ASTRO, MAUVE),
    group(&["graphql", "gql"], glyph::GRAPHQL, PINK),
    group(&["prisma"], glyph::PRISMA, TEAL),
    group(&["ejs"], glyph::EJS, GREEN),
    group(&["pug", "jade"], glyph::PUG, ROSEWATER),
    group(&["hbs", "handlebars", "mustache"], glyph::MUSTACHE, PEACH),
    group(&["twig"], glyph::TWIG, GREEN),
    group(&["liquid"], glyph::LIQUID, BLUE),
    group(&["jinja", "jinja2", "j2"], glyph::JINJA, RED),
    group(&["haml"], glyph::HAML, YELLOW),
    // Data and configuration.
    group(
        &["json", "jsonc", "json5", "jsonl", "ndjson", "webmanifest"],
        glyph::JSON,
        YELLOW,
    ),
    group(&["map", "tsbuildinfo"], glyph::JSON, OVERLAY1),
    group(&["yaml", "yml"], glyph::YAML, RED),
    group(&["toml"], glyph::TOML, MAROON),
    group(&["lock"], glyph::LOCK, OVERLAY1),
    group(
        &[
            "xml",
            "xsd",
            "xsl",
            "xslt",
            "dtd",
            "plist",
            "iml",
            "storyboard",
            "xib",
        ],
        glyph::XML,
        PEACH,
    ),
    group(&["proto"], glyph::STRUCTURE, RED),
    group(&["csv", "tsv", "psv"], glyph::CSV, GREEN),
    group(
        &["sql", "sqlite", "sqlite3", "db", "psql", "pgsql"],
        glyph::DATABASE,
        YELLOW,
    ),
    group(
        &["ini", "cfg", "conf", "config", "cnf", "properties", "prefs"],
        glyph::CONFIG,
        TEXT,
    ),
    group(&["env", "envrc"], glyph::KEY, YELLOW),
    group(&["editorconfig"], glyph::EDITORCONFIG, ROSEWATER),
    group(
        &[
            "gitignore",
            "gitattributes",
            "gitmodules",
            "gitkeep",
            "gitconfig",
            "gitmessage",
            "keep",
            "mailmap",
        ],
        glyph::GIT,
        PEACH,
    ),
    group(&["patch"], glyph::DIFF, PEACH),
    group(&["diff"], glyph::DIFF, GREEN),
    group(
        &["pem", "key", "pub", "asc", "gpg", "p12", "pfx"],
        glyph::KEY,
        TEXT,
    ),
    group(&["crt", "cer", "cert"], glyph::CERTIFICATE, RED),
    // Documents.
    group(&["md", "markdown", "rst"], glyph::MARKDOWN, SAPPHIRE),
    group(&["mdx"], glyph::MARKDOWN, PEACH),
    group(&["txt", "text"], glyph::TEXT, TEXT),
    group(&["log"], glyph::LOG, TEXT),
    group(&["pdf"], glyph::PDF, RED),
    group(&["tex", "sty", "cls", "bib"], glyph::TEX, TEXT),
    group(&["doc", "docx", "rtf", "odt"], glyph::WORD, BLUE),
    group(&["xls", "xlsx", "ods"], glyph::EXCEL, GREEN),
    group(&["ppt", "pptx", "odp"], glyph::POWERPOINT, PEACH),
    // Media.
    group(
        &[
            "png", "jpg", "jpeg", "gif", "webp", "ico", "bmp", "tif", "tiff", "avif", "heic",
            "heif", "icns",
        ],
        glyph::IMAGE,
        GREEN,
    ),
    group(&["svg"], glyph::SVG, PEACH),
    group(
        &[
            "mp3", "wav", "flac", "aac", "ogg", "oga", "m4a", "opus", "aiff", "wma",
        ],
        glyph::AUDIO,
        MAROON,
    ),
    group(
        &[
            "mp4", "mov", "mkv", "webm", "avi", "m4v", "mpeg", "mpg", "wmv", "flv",
        ],
        glyph::VIDEO,
        SAPPHIRE,
    ),
    group(
        &["ttf", "otf", "woff", "woff2", "eot", "ttc"],
        glyph::FONT,
        FLAMINGO,
    ),
    // Archives and binaries.
    group(
        &[
            "zip", "tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zst", "7z", "rar", "z",
            "lz4", "dmg", "pkg", "deb", "rpm", "whl",
        ],
        glyph::ARCHIVE,
        TEXT,
    ),
    group(
        &[
            "exe", "dll", "so", "dylib", "a", "o", "obj", "bin", "lib", "rlib", "rmeta", "out",
            "msi",
        ],
        glyph::BINARY,
        TEXT,
    ),
];

/// Directory names that get an accent colour, lower case.
static DIR_NAMES: &[(&[&str], Rgb)] = &[
    (
        &[
            "src",
            "srcs",
            "source",
            "sources",
            "code",
            "node_modules",
            "components",
            "widgets",
            "utils",
            "util",
            "utilities",
            "middleware",
            "middlewares",
            "routes",
            "router",
            "routers",
            "images",
            "image",
            "img",
            "imgs",
            "icons",
            "screenshots",
            "android",
        ],
        GREEN,
    ),
    (
        &[
            "test",
            "tests",
            "testing",
            "spec",
            "specs",
            "__tests__",
            "__test__",
            "__mocks__",
            "__fixtures__",
            "__snapshots__",
            "types",
            "typings",
            "@types",
            "tmp",
            "temp",
            "cache",
            ".cache",
            ".tmp",
            ".temp",
            "prisma",
        ],
        TEAL,
    ),
    (
        &[
            "doc",
            "docs",
            "documentation",
            "documents",
            "app",
            "apps",
            "client",
            "clients",
            "frontend",
            "docker",
            ".docker",
            ".vscode",
            "styles",
            "style",
            "css",
            "plugins",
            "plugin",
            "extensions",
            "addons",
            "modules",
            "mods",
            "functions",
            "lambda",
            "layouts",
            "layout",
            "admin",
            "k8s",
            "kubernetes",
            "macos",
            "windows",
            "video",
            "videos",
            "xcodeproj",
            "xcworkspace",
            "xcshareddata",
        ],
        BLUE,
    ),
    (
        &[
            "dist", "build", "out", "bin", "release", ".output", "debug", "private",
        ],
        RED,
    ),
    (
        &[
            ".git",
            ".githooks",
            "githooks",
            "patches",
            "submodules",
            ".cargo",
            "crate",
            "crates",
            "package",
            "packages",
            "pkg",
            "pkgs",
            "bench",
            "benches",
            "benchmark",
            "benchmarks",
            "workflows",
            "workflow",
            "ci",
            ".ci",
            ".gitlab",
            "controllers",
            "handlers",
            "services",
            "providers",
            "views",
            "pages",
            "screens",
            "aws",
            ".aws",
            "svg",
            "svgs",
        ],
        PEACH,
    ),
    (
        &[
            ".github",
            "github",
            "config",
            "configs",
            ".config",
            "cfg",
            "conf",
            "settings",
            "options",
            "lib",
            "libs",
            "library",
            "libraries",
            "core",
            "constants",
            "cli",
            "cmd",
            "command",
            "commands",
            ".next",
            ".vercel",
            ".circleci",
            "linux",
        ],
        OVERLAY1,
    ),
    (
        &[
            "scripts",
            "script",
            "hooks",
            "hook",
            "locales",
            "locale",
            "i18n",
            "l10n",
            "lang",
            "languages",
            "translations",
            ".idea",
            ".yarn",
            "redux",
            "composables",
        ],
        MAUVE,
    ),
    (
        &[
            "assets",
            "asset",
            "api",
            "apis",
            "examples",
            "example",
            "demo",
            "demos",
            "samples",
            "sample",
            "data",
            "db",
            "database",
            "databases",
            "sql",
            "server",
            "servers",
            "backend",
            "firebase",
            ".firebase",
            "js",
            "javascript",
        ],
        YELLOW,
    ),
    (
        &[
            "public", "www", "web", "site", "website", "_site", "include", "includes", "nix",
            "gradle", ".gradle",
        ],
        SAPPHIRE,
    ),
    (
        &[
            "mocks",
            "mock",
            "fixtures",
            "fixture",
            "drafts",
            "content",
            "contents",
            "cloud",
            "downloads",
            "download",
            "uploads",
            "upload",
            "queue",
            "queues",
            ".devcontainer",
        ],
        SKY,
    ),
    (
        &[
            "shared",
            "share",
            "animations",
            "animation",
            "graphql",
            "gql",
            "sass",
            "scss",
            "stories",
            ".storybook",
            "storybook",
        ],
        PINK,
    ),
    (
        &["fonts", "font", "security", "messages", "message"],
        FLAMINGO,
    ),
    (
        &["audio", "sounds", "sound", "music", "templates", "template"],
        MAROON,
    ),
    (&["themes", "theme", ".husky", "husky"], ROSEWATER),
    (&[".azure-pipelines", ".azuredevops"], LAVENDER),
];

fn lookup(table: &'static [Group]) -> HashMap<&'static str, Icon> {
    let mut map = HashMap::new();
    for group in table {
        for name in group.names {
            map.insert(
                *name,
                Icon {
                    glyph: group.glyph,
                    color: group.color,
                },
            );
        }
    }
    map
}

fn file_names() -> &'static HashMap<&'static str, Icon> {
    static MAP: OnceLock<HashMap<&'static str, Icon>> = OnceLock::new();
    MAP.get_or_init(|| lookup(FILE_NAMES))
}

fn extensions() -> &'static HashMap<&'static str, Icon> {
    static MAP: OnceLock<HashMap<&'static str, Icon>> = OnceLock::new();
    MAP.get_or_init(|| lookup(EXTENSIONS))
}

fn dir_colors() -> &'static HashMap<&'static str, Rgb> {
    static MAP: OnceLock<HashMap<&'static str, Rgb>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for (names, color) in DIR_NAMES {
            for name in *names {
                map.insert(*name, *color);
            }
        }
        map
    })
}

/// Whether `name` is `prefix` on its own or `prefix` followed by one of `separators`
/// (`license`, `license.md`, `license-mit`).
fn is_family(name: &str, prefix: &str, separators: &[char]) -> bool {
    match name.strip_prefix(prefix) {
        Some("") => true,
        Some(rest) => rest.starts_with(separators),
        None => false,
    }
}

/// Icons for families of names that share a prefix rather than an extension.
fn family_icon(name: &str) -> Option<Icon> {
    if name.starts_with(".env") {
        return Some(ENV);
    }
    if name.starts_with(".git") {
        return Some(GIT);
    }
    if is_family(name, "dockerfile", &['.']) || is_family(name, "containerfile", &['.']) {
        return Some(DOCKER);
    }
    if (name.starts_with("docker-compose") || is_family(name, "compose", &['.']))
        && (name.ends_with(".yml") || name.ends_with(".yaml"))
    {
        return Some(DOCKER_COMPOSE);
    }
    for prefix in ["license", "licence", "unlicense", "copying", "copyright"] {
        if is_family(name, prefix, &['.', '-', '_']) {
            return Some(LICENSE);
        }
    }
    if is_family(name, "readme", &['.', '-', '_']) {
        return Some(README);
    }
    if is_family(name, "changelog", &['.']) || is_family(name, "changes", &['.']) {
        return Some(CHANGELOG);
    }
    if name.starts_with(".eslintrc") || name.starts_with("eslint.config.") {
        return Some(ESLINT);
    }
    if name.starts_with(".prettierrc") || name.starts_with("prettier.config.") {
        return Some(PRETTIER);
    }
    if is_family(name, "tsconfig", &['.']) && name.ends_with(".json") {
        return Some(TSCONFIG);
    }
    None
}

fn file_icon(name: &str) -> Icon {
    let lower = name.to_lowercase();
    if let Some(icon) = file_names().get(lower.as_str()) {
        return *icon;
    }
    if let Some(icon) = family_icon(&lower) {
        return icon;
    }
    lower
        .rsplit_once('.')
        .and_then(|(_, extension)| extensions().get(extension))
        .copied()
        .unwrap_or(DEFAULT_FILE)
}

fn dir_color(name: &str) -> Rgb {
    dir_colors()
        .get(name.to_lowercase().as_str())
        .copied()
        .unwrap_or(TEXT)
}

/// The Nerd Font icon for a file or directory called `name`.
///
/// `name` is the entry's own name, not a path; matching ignores case. `expanded` only
/// matters for directories and picks the open folder. Unknown files get a plain file
/// glyph, and every glyph is one cell wide in a Nerd Font.
pub fn icon_for(name: &str, is_dir: bool, expanded: bool) -> Icon {
    if is_dir {
        return Icon {
            glyph: if expanded {
                glyph::FOLDER_OPEN
            } else {
                glyph::FOLDER
            },
            color: dir_color(name),
        };
    }
    file_icon(name)
}

/// The icon to draw when the terminal font has no Nerd Font glyphs: `▸` for a collapsed
/// directory, `▾` for an expanded one and `·` for a file, in the same colours
/// [`icon_for`] would use.
pub fn ascii_icon_for(name: &str, is_dir: bool, expanded: bool) -> Icon {
    let glyph = match (is_dir, expanded) {
        (true, true) => ascii::FOLDER_OPEN,
        (true, false) => ascii::FOLDER,
        (false, _) => ascii::FILE,
    };
    Icon {
        glyph,
        color: icon_for(name, is_dir, expanded).color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use unicode_width::UnicodeWidthStr;

    fn file(name: &str) -> Icon {
        icon_for(name, false, false)
    }

    /// Every icon the module can return.
    fn every_icon() -> Vec<Icon> {
        let mut icons = vec![DEFAULT_FILE];
        icons.extend([
            DOCKER,
            DOCKER_COMPOSE,
            ENV,
            GIT,
            LICENSE,
            README,
            CHANGELOG,
            ESLINT,
            PRETTIER,
            TSCONFIG,
        ]);
        for table in [FILE_NAMES, EXTENSIONS] {
            for group in table {
                icons.push(Icon {
                    glyph: group.glyph,
                    color: group.color,
                });
            }
        }
        for (_, color) in DIR_NAMES {
            for glyph in [glyph::FOLDER, glyph::FOLDER_OPEN] {
                icons.push(Icon {
                    glyph,
                    color: *color,
                });
            }
        }
        for glyph in [ascii::FOLDER, ascii::FOLDER_OPEN, ascii::FILE] {
            icons.push(Icon { glyph, color: TEXT });
        }
        icons
    }

    #[test]
    fn every_glyph_is_one_cell_wide() {
        for icon in every_icon() {
            assert_eq!(
                icon.glyph.chars().count(),
                1,
                "{:?} is not a single code point",
                icon.glyph
            );
            assert_eq!(
                UnicodeWidthStr::width(icon.glyph),
                1,
                "{:?} (U+{:04X}) is not one cell wide",
                icon.glyph,
                icon.glyph.chars().next().unwrap() as u32
            );
        }
    }

    #[test]
    fn nerd_font_glyphs_are_private_use_code_points() {
        for table in [FILE_NAMES, EXTENSIONS] {
            for group in table {
                let code = group.glyph.chars().next().unwrap() as u32;
                assert!(
                    (0xe000..=0xf8ff).contains(&code),
                    "U+{code:04X} for {:?} is outside the private use area",
                    group.names
                );
            }
        }
    }

    #[test]
    fn table_keys_are_lower_case_and_unique() {
        for table in [FILE_NAMES, EXTENSIONS] {
            let mut seen = HashSet::new();
            for group in table {
                for name in group.names {
                    assert_eq!(*name, name.to_lowercase(), "{name:?} is not lower case");
                    assert!(seen.insert(*name), "{name:?} is listed twice");
                }
            }
        }
        let mut seen = HashSet::new();
        for (names, _) in DIR_NAMES {
            for name in *names {
                assert_eq!(*name, name.to_lowercase(), "{name:?} is not lower case");
                assert!(seen.insert(*name), "directory {name:?} is listed twice");
            }
        }
    }

    #[test]
    fn the_listed_file_types_all_have_their_own_icon() {
        let extensions = [
            "rs", "toml", "lock", "py", "js", "mjs", "cjs", "jsx", "ts", "tsx", "json", "jsonc",
            "yaml", "yml", "md", "mdx", "html", "css", "scss", "sh", "bash", "zsh", "fish", "go",
            "c", "h", "cpp", "hpp", "swift", "kt", "java", "rb", "lua", "sql", "tf", "hcl", "nix",
            "gd", "tscn", "png", "jpg", "jpeg", "gif", "svg", "webp", "ico", "pdf", "zip", "tar",
            "gz", "txt", "log", "csv", "xml", "proto", "graphql", "vue", "svelte", "dart", "php",
            "cs", "zig", "ipynb",
        ];
        for extension in extensions {
            let name = format!("example.{extension}");
            assert_ne!(file(&name), DEFAULT_FILE, "{name} has no icon");
        }
        let names = [
            "Dockerfile",
            "Makefile",
            "Justfile",
            "justfile",
            ".gitignore",
            ".gitattributes",
            ".gitmodules",
            ".gitkeep",
            ".env",
            ".env.local",
            "LICENSE",
            "LICENSE-MIT",
            "README",
            "README.md",
        ];
        for name in names {
            assert_ne!(file(name), DEFAULT_FILE, "{name} has no icon");
        }
    }

    #[test]
    fn colours_follow_the_catppuccin_theme() {
        assert_eq!(file("main.rs").color, PEACH);
        assert_eq!(file("Cargo.toml").color, PEACH);
        assert_eq!(file("settings.toml").color, MAROON);
        assert_eq!(file("app.py").color, BLUE);
        assert_eq!(file("index.js").color, YELLOW);
        assert_eq!(file("App.jsx").color, SKY);
        assert_eq!(file("index.ts").color, BLUE);
        assert_eq!(file("package.json").color, GREEN);
        assert_eq!(file("data.json").color, YELLOW);
        assert_eq!(file("ci.yml").color, RED);
        assert_eq!(file("notes.md").color, SAPPHIRE);
        assert_eq!(file("page.mdx").color, PEACH);
        assert_eq!(file("style.css").color, MAUVE);
        assert_eq!(file("style.scss").color, PINK);
        assert_eq!(file("run.sh").color, GREEN);
        assert_eq!(file("main.go").color, SAPPHIRE);
        assert_eq!(file("main.c").color, BLUE);
        assert_eq!(file("main.h").color, MAUVE);
        assert_eq!(file("player.gd").color, SKY);
        assert_eq!(file("level.tscn").color, GREEN);
        assert_eq!(file("Dockerfile").color, BLUE);
        assert_eq!(file(".env").color, YELLOW);
        assert_eq!(file("LICENSE").color, PEACH);
        assert_eq!(file("README.md").color, TEAL);
        assert_eq!(file("photo.png").color, GREEN);
        assert_eq!(file("report.pdf").color, RED);
        assert_eq!(file("build.zig").color, YELLOW);
    }

    #[test]
    fn whole_names_beat_extensions() {
        assert_eq!(file("Cargo.lock").glyph, glyph::LOCK);
        assert_eq!(file("Cargo.lock").color, PEACH);
        assert_eq!(file("other.lock").color, OVERLAY1);
        assert_eq!(file("package.json").glyph, glyph::NPM);
        assert_eq!(file("tsconfig.json"), TSCONFIG);
        assert_eq!(file("tsconfig.build.json"), TSCONFIG);
        assert_eq!(file("docker-compose.yml"), DOCKER_COMPOSE);
        assert_eq!(file("compose.override.yaml"), DOCKER_COMPOSE);
        assert_eq!(file("CMakeLists.txt").glyph, glyph::CMAKE);
    }

    #[test]
    fn matching_ignores_case() {
        assert_eq!(file("MAIN.RS"), file("main.rs"));
        assert_eq!(file("readme.MD"), file("README.md"));
        assert_eq!(file("makefile"), file("Makefile"));
        assert_eq!(file("DOCKERFILE"), file("Dockerfile"));
        assert_eq!(icon_for("SRC", true, false), icon_for("src", true, false));
    }

    #[test]
    fn families_match_only_on_a_separator() {
        assert_eq!(file("Dockerfile.dev"), DOCKER);
        assert_eq!(file("LICENSE.txt"), LICENSE);
        assert_eq!(file("COPYING"), LICENSE);
        assert_eq!(file("README_fr.md"), README);
        assert_eq!(file("CHANGELOG.md"), CHANGELOG);
        assert_eq!(file(".env.production"), ENV);
        assert_eq!(file(".eslintrc.json"), ESLINT);
        assert_eq!(file(".prettierrc"), PRETTIER);
        // Not part of the family: falls through to the extension.
        assert_eq!(file("licenses.rs").glyph, glyph::RUST);
        assert_eq!(file("readmefirst.txt").glyph, glyph::TEXT);
        assert_eq!(file("composer.json").glyph, glyph::JSON);
    }

    #[test]
    fn dotfiles_are_matched_by_the_name_after_the_dot() {
        assert_eq!(file(".zshrc").glyph, glyph::TERMINAL);
        assert_eq!(file(".bashrc").glyph, glyph::TERMINAL);
        assert_eq!(file(".vimrc").glyph, glyph::VIM);
        assert_eq!(file(".editorconfig").glyph, glyph::EDITORCONFIG);
        assert_eq!(file(".gitignore"), GIT);
    }

    #[test]
    fn unknown_and_odd_names_get_the_default_file() {
        assert_eq!(file("mystery"), DEFAULT_FILE);
        assert_eq!(file("archive.unknownext"), DEFAULT_FILE);
        assert_eq!(file(""), DEFAULT_FILE);
        assert_eq!(file("."), DEFAULT_FILE);
        assert_eq!(file("trailing."), DEFAULT_FILE);
        assert_eq!(file("ünïcödé"), DEFAULT_FILE);
    }

    #[test]
    fn only_the_last_extension_counts() {
        assert_eq!(file("bundle.tar.gz").glyph, glyph::ARCHIVE);
        assert_eq!(file("types.d.ts").glyph, glyph::TYPESCRIPT);
        assert_eq!(file("app.test.tsx").glyph, glyph::REACT);
        assert_eq!(file("icon.svg.import").glyph, glyph::GODOT);
    }

    #[test]
    fn folders_open_and_close() {
        let closed = icon_for("anything", true, false);
        let open = icon_for("anything", true, true);
        assert_eq!(closed.glyph, glyph::FOLDER);
        assert_eq!(open.glyph, glyph::FOLDER_OPEN);
        assert_eq!(closed.color, TEXT);
        assert_eq!(icon_for("src", true, false).color, GREEN);
        assert_eq!(icon_for("tests", true, true).color, TEAL);
        assert_eq!(icon_for("docs", true, false).color, BLUE);
        // A directory named like a file type is still a folder.
        assert_eq!(icon_for("main.rs", true, false).glyph, glyph::FOLDER);
    }

    #[test]
    fn ascii_icons_keep_the_colours() {
        assert_eq!(ascii_icon_for("src", true, false).glyph, "▸");
        assert_eq!(ascii_icon_for("src", true, true).glyph, "▾");
        assert_eq!(ascii_icon_for("main.rs", false, false).glyph, "·");
        assert_eq!(ascii_icon_for("main.rs", false, false).color, PEACH);
        assert_eq!(ascii_icon_for("src", true, true).color, GREEN);
    }
}
