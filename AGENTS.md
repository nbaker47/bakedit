# bakedit

A lean terminal IDE modelled on Nathan's Zed setup: terminal and git on the left, editor in
the middle, file tree on the right, Zed's keybindings, a nav/edit mode pair instead of vim,
and herdr running in the terminal panel. No AI features, no collaboration, no extensions.

## Layout

One Cargo workspace. Each crate has one job and depends only on `bakedit-core` (plus its
own third-party crates); only the app depends on the others.

| Crate | Package | Job |
|---|---|---|
| `crates/core` | `bakedit-core` | Shared types: `KeyEvent`, `MouseEvent`, `InputEvent`, `Action`, `Rgb`, `Theme`, JSON-with-comments |
| `crates/input` | `bakedit-input` | Bytes from the host terminal → `InputEvent`s (kitty keyboard protocol, mouse, paste); the built-in nav layer |
| `crates/keymap` | `bakedit-keymap` | Zed-format keymaps: keystrokes, context predicates, binding resolution, pending chords |
| `crates/text` | `bakedit-text` | Rope buffer, selections, motions, edits, undo, in-buffer search |
| `crates/syntax` | `bakedit-syntax` | Language registry and tree-sitter highlighting |
| `crates/term` | `bakedit-term` | Embedded terminal: pty, VT emulation, key/mouse encoding, link detection |
| `crates/git` | `bakedit-git` | Git through the `git` command line: status, diff, stage, commit, permalinks |
| `crates/project` | `bakedit-project` | File tree model, file index + fuzzy finder, project search, file watching, icons |
| `crates/herdr` | `bakedit-herdr` | herdr socket API client and native pane navigation |
| `crates/app` | `bakedit` | The binary: workspace, docks, panels, editor view, pickers, config, IPC |

`assets/themes/ghostty.json` is the built-in theme (the Zed "Ghostty" theme).
`assets/keymaps/default.json` is the built-in keymap, in Zed's keymap format.

## Build and test

```
cargo build                       # everything
cargo test -p bakedit-text        # one crate
cargo clippy --workspace --all-targets
cargo run -p bakedit -- <path>    # run the IDE
```

Worktrees share one build directory so dependencies compile once:
`export CARGO_TARGET_DIR=/Users/user/Code/bakedit/target`.

## The app (`crates/app`)

A library (`src/lib.rs`) plus a thin binary, so tests and examples can drive it.

- **Shell and views.** `view.rs` is the contract: a `View` draws into a `Canvas`, answers
  `Action`s and unclaimed keys, and reports its key `Context`s. An `Item` is a view that lives
  in a tab. Views never touch each other; they push an `Effect` (open a file, move the focus,
  ask a question) and the shell applies it after the view returns.
- **Model, no drawing:** `workspace.rs` (docks, focus, zoom: a port of Zed's rules, read its
  header), `pane.rs` (tabs, preview tab, closed-tab stack, back/forward), `layout.rs`
  (rectangles for docks, centre, dividers, status bar), `settings.rs` (Zed's settings.json
  keys), `session.rs`, `ipc.rs` (one socket per running window), `cli.rs`.
- **Drawing, pure functions of data:** `ui.rs` (clipped `Canvas`, `Style`, truncation),
  `chrome.rs` (tab bar, status bar, prompt, toast), `picker.rs`, `line_input.rs`, `paint.rs`
  (one editor row: syntax, selections, matches, brackets, indent guides; gutter; scrollbar),
  `minimap.rs`, `hints.rs` (which-key), `usage.rs` (the usage strip).
- **Keymap.** `assets/keymaps/default.json` is Zed's format and semantics. Action names are
  Zed's where Zed has the action; bakedit's own are `bakedit::*`, `herdr::*`,
  `project_diff::*`, `terminal_panel::ToggleZoom`, `terminal::OpenLinks`,
  `editor::SelectBetween`, `editor::MoveToNextWordStart`, `editor::MoveToFirstNonBlank`.
- **Key context stacks** (root first), which the keymap's sections rely on:

  | Focus | Stack |
  |---|---|
  | Editor | `Workspace > Pane > Editor{mode=full, vim_mode=normal\|insert, extension}` |
  | Find bar | `Workspace > Pane > BufferSearchBar{in_replace} > Editor{mode=single_line}` |
  | Project search | `Workspace > Pane > ProjectSearchView`, query: `... > ProjectSearchBar{in_replace} > Editor` |
  | Project diff | `Workspace > Pane > ProjectDiff` |
  | Terminal | `Workspace > Dock > Pane > Terminal` |
  | File tree | `Workspace > Dock > ProjectPanel{not_editing\|editing}` (`> Editor` while renaming) |
  | Git panel | `Workspace > Dock > GitPanel{ChangesList}` or `... > GitPanel{CommitEditor} > CommitEditor > Editor{mode=auto_height}` |
  | Pickers | `Workspace > FileFinder\|CommandPalette\|TabSwitcher\|... > Picker > Editor{mode=single_line}` |
  | Prompt | `Workspace > Prompt` |

- **Looking at it.** A frame can be turned into a picture without a terminal:
  `cargo run -p bakedit --example mock_frame -- frame.html 170 46 [--picker]`, then
  `"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --screenshot=frame.png --window-size=1500,820 --force-device-scale-factor=2 --user-data-dir=<scratch dir> file://.../frame.html`.
  `dump::to_text` gives the same frame as text for assertions. Check spacing, alignment and
  empty states in the picture before calling UI work done.
- Tests must never touch the user's clipboard (`BAKEDIT_CLIPBOARD=memory` or a
  `MemoryBackend`), their herdr session, or their config files.

## Conventions

- Rust 2021, stable toolchain. No `unsafe` unless a system call needs it, and then with a
  comment saying why it is sound.
- Library crates never panic on bad input (a file, a key sequence, a git reply, a socket
  reply): return a `Result` or a sensible default. `unwrap`/`expect` only where the
  invariant is local and obvious.
- Library crates do not depend on ratatui or crossterm and do not print. They return data;
  the app draws it.
- Every public function has a doc comment that says what it does and what the edge cases
  do. Behaviour is covered by unit tests next to the code; anything that touches the
  filesystem, a pty, git or a socket has an integration test that uses a temp directory or
  a throwaway process, never the user's real repos or their running herdr session.
- Keep the code plain: small functions, descriptive names, no speculative abstraction.
- Positions in text are `char` offsets into the rope unless a name says otherwise
  (`byte_`, `line`, `col`, `display_col`).
- Screen coordinates are 0-based `(col, row)` cells.
- Action names follow Zed (`editor::MoveUp`, `terminal_panel::Toggle`).
