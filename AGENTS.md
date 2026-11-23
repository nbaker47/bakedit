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
cargo run -p bakedit -- <path>    # run the IDE (in a Ghostty window; --here for this terminal)
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
- **Shared state and background work:** `project.rs` (`Project`, owned by the shell and lent
  to every view as `cx.project`: the git repository and status, `git_job` for commands off
  the UI thread, `git_base` for the gutter, the file index and watcher, recent files and
  search history, the syntax thread), `background.rs` (`Waker`, `Task`). Nothing a view does
  may block drawing: start work with `cx.project.spawn` or `git_job` and collect it in `tick`.
- **The running app:** `app.rs` (the main loop: input wiring, timers, drawing, session
  saves, config reloads), `host.rs` (terminal modes and their restore), `main.rs` (every
  `cli.rs` mode), `window.rs` (the dedicated Ghostty window a plain `bakedit` opens on
  macOS, and `decide`: that window, the running one for files, or this terminal with
  `--here` or `"window": false`; the bakedit in the window gets `--here` and
  `BAKEDIT_HOST`, so it never opens another), `themes.rs`,
  `logging.rs` (`--log`), `external.rs` (URLs and Finder reveals handed to the OS; test
  builds record them instead, so no test opens a window).
- **Views:** `editor/` (`EditorItem`), `panels/terminal.rs`, `panels/git.rs`,
  `panels/project.rs`, `items/diff.rs`, `items/search.rs`; `factory.rs` makes the items and
  fills the file finder, branch and recent-project pickers. Each module's header states the
  constructor and methods the app calls; keep those signatures.
- **Driving the real binary:** `crates/app/tests/scenarios/run.sh <out-dir> [bakedit]
  [scenario ...]` builds a throwaway git project (with a Cargo.toml for format on save and a
  50,000-line file) and runs `<scenario>.keys` scripts through `scripts/ptyshot.py`, a pty
  and a VT emulator that answers as Ghostty (or, with `--legacy`, a terminal without the
  kitty protocol). Each script's `expect`, `expect-not`, `expect-at`, `expect-exit` and
  `exec` lines are its checks; run.sh prints PASS or FAIL per scenario and keeps text, HTML
  and PNG snapshots (`<scenario>-failed.*` when one fails). `zed-settings.json` is Nathan's
  editor settings, put where bakedit looks for Zed's. `herdr-session` drives a real herdr
  through a throwaway server of its own (`crates/herdr/tests/isolated-session.sh`,
  `herdr-check.py`); `quit` checks the terminal is restored (`check-restored.py`).
  `tests/scenarios.rs` replays them under `cargo test` (skipped without `uv`). run.sh gives
  every bakedit `--here`, and a bakedit in ptyshot (`TERM_PROGRAM=ptyshot`) never opens a
  Ghostty window whatever it is given. Run bakedit by hand the same way:
  `--here` (without it bakedit opens a Ghostty window and takes the user's focus),
  `--terminal shell` (never the user's herdr session), `XDG_CONFIG_HOME` and
  `XDG_STATE_HOME` in a scratch folder, `ZDOTDIR` there too so the panel's shell reads none
  of the user's dotfiles.
- **Every key does something:** `handlers_tests.rs` builds the real app in each place the
  keymap's contexts describe and fails on any action the keymap binds or the palette lists
  that nothing acts on. A binding added without a handler fails it; a deliberate no-op goes
  in its `DELIBERATE_NO_OPS` with the reason.
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
  `MemoryBackend`), their herdr session, or their config files, and never open a Ghostty
  window (test `window::decide` and `window::window_command`, run the binary with `--here`).

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
