# bakedit

A fast, lean terminal IDE in Rust, built around [herdr](https://github.com/ogulcancelik/herdr)
and a keyboard-first nav layer.

![bakedit: editor, minimap, file tree and git gutter in one terminal window](docs/img/kitty-editor.png)

## Why

I used Zed for a long time and loved the speed and the keybindings. Then it filled up with AI
features, agent panels, collaboration, extensions and a growing pile of things I never asked
for. I wanted the editor back, and nothing else.

bakedit is that editor. It does not have:

- AI assistants, agent panels or inline completions (my agents run in herdr, in a terminal, where they belong)
- collaboration, calls or accounts
- an extension system or a marketplace
- a GUI toolkit, a web view or an Electron shell

It does have the parts I use every day: a rope-based text buffer, tree-sitter highlighting, a
minimap, find and project search, a file tree, a git panel with a project diff, an embedded
terminal, command palette, tab switcher, and Zed's keymap and `settings.json` keys so my
muscle memory carries over.

## Built around herdr

The terminal panel runs [herdr](https://github.com/ogulcancelik/herdr), a terminal workspace
manager, by default. bakedit speaks to its socket, so herdr's panes, workspaces and zoom are
native actions in the editor: `cmd-alt-arrows` moves focus between herdr panes (hopping to the
next tab or workspace at an edge), `cmd-alt-1` to `9` jumps workspaces, and
`ctrl-]` gives a prefix that works even where the terminal cannot pass `cmd` through.

It is a single Rust binary. Everything runs in your terminal, so it works over SSH, in
Ghostty, cmux, kitty or anything that speaks the kitty keyboard protocol, and it falls back
gracefully where one does not.

![The window: terminal and git on the left, editor in the middle, file tree on the right](docs/img/layout.svg)

![Terminal panel, editor and file tree side by side](docs/img/panels-three-columns.png)

## The nav layer

Vim has the right idea: your fingers should stay on the home row. But modal editing only
helps inside the editor. The moment you reach for an arrow key, a modifier, or a shortcut in
any other panel, your hand leaves the keys.

bakedit builds the fix in. **Hold `;`** and the home row becomes a layer:

- `e` `s` `d` `f` are up, left, down, right
- `h` `j` `k` `l` are ctrl, shift, cmd, alt, and they stack
- `a` is tab and `c` is escape
- every other key passes through with the layer's modifiers added
- tap `;` on its own and it types `;`

![The nav layer on a QWERTY keyboard](docs/img/nav-layer.svg)

Because the layer's modifiers add to any key, every shortcut in every panel is reachable
without moving off the home row. `;` + `k` + `p` is cmd-p. `;` + `k` + `j` + `e` selects to the
top of the file. It works in the editor, the terminal panel, the file tree, the git panel and
every picker, because it lives below all of them.

![Example nav-layer chords](docs/img/nav-combos.svg)

It is much faster than vim's approach in practice: there is no mode to be in, no mode to
forget you are in, and the same arrows and shortcuts work everywhere. Under the layer is the
usual pair of modes if you want them:

![NAV and INSERT modes](docs/img/modes.svg)

The layer matches the physical key position, not the character, so it stays in place on
other keyboard layouts. It is a built-in version of the Karabiner-Elements rule I used to
run, so it works wherever bakedit runs. Turn it off with `"nav_layer": false` in settings
or the `bakedit::ToggleNavLayer` action. The key assignments live in `NavLayerConfig`
(`crates/input/src/nav.rs`).

## What it looks like

Fuzzy file finder (`cmd-p`):

![File finder](docs/img/kitty-finder.png)

Project search with include and exclude globs (`cmd-shift-f`):

![Project search](docs/img/search-project-search.png)

Git panel with stage, commit and amend (`cmd-g`), and a project diff (`space d`):

![Git panel](docs/img/kitty-git.png)

## Install

You need a recent stable Rust toolchain, `git`, and optionally [herdr](https://github.com/ogulcancelik/herdr)
for the terminal panel.

```sh
git clone https://github.com/nbaker47/bakedit
cd bakedit
cargo build --release
ln -s "$PWD/target/release/bakedit" ~/.local/bin/bakedit
```

```sh
bakedit                      # open the current directory
bakedit src/main.rs:42       # open a file at a line (also :42:7, :42-60, #L42)
bakedit --window             # a dedicated Ghostty window that passes every cmd key through
bakedit --terminal shell     # plain shell in the terminal panel instead of herdr
bakedit --probe              # what your terminal supports
bakedit --print-keymap       # the built-in keymap
```

When bakedit is already running, files are sent to that window, so it works as `$EDITOR`
and as the target of `path:line` links printed by tools and agents. Terminal output links
are cmd-clickable too.

## Keys

The keymap is Zed's format and semantics: `assets/keymaps/default.json`, with Zed's action
names where Zed has the action. A few that matter most:

| Key | Does |
|---|---|
| `cmd-p` | file finder |
| `cmd-shift-p` | command palette |
| `cmd-shift-f` | project search |
| `cmd-t` / `cmd-g` / `cmd-shift-e` | terminal / git / file tree |
| `cmd-e` | back to the editor |
| `cmd-alt-f` | hide every dock |
| `ctrl-]` then `?` | show all keybindings |
| `space f`, `space /`, `space g` (NAV mode) | find file, search, git |

In NAV mode the editor answers to `h j k l`, `w b e`, `gg`, `G`, `y d c p x u`, `v`, `/`, `n`
and friends, so it feels like a small subset of vim without needing to be one.

## Built to stay fast

- One Cargo workspace of small crates, each with one job: `input`, `keymap`, `text`, `syntax`,
  `term`, `git`, `project`, `herdr`, and the `app` that wires them together.
- Nothing a view does may block drawing. Git, search, indexing and syntax run on background
  threads and report back on the next tick.
- Libraries never panic on bad input and never print.
- About 2,300 tests, including scenarios that drive the real release binary through a
  pseudo-terminal and check the screen (`crates/app/tests/scenarios/run.sh`).

## Status

Early, and built for one user's workflow first. It is my daily driver, but expect rough edges
(tree drag and multi-select, hunk staging and a few vim-ish quirks are not there yet). Issues
and pull requests are welcome, but there is no promise to add features I do not use.

`AGENTS.md` has the architecture notes and how to build, test and screenshot it.
`python3 scripts/gen_diagrams.py` regenerates the SVG diagrams in this README.

## Credits

The look and keymap follow [Zed](https://zed.dev), and the terminal workspace is
[herdr](https://github.com/ogulcancelik/herdr).
