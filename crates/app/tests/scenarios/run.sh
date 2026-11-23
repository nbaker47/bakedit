#!/usr/bin/env bash
# Drive the real bakedit binary through scripts/ptyshot.py and keep what its screen showed.
#
#   crates/app/tests/scenarios/run.sh <out-dir> [path/to/bakedit]
#
# Runs on a throwaway git project: kitty.keys (a Ghostty-like terminal, 200x52, including a
# cmd+click on a path printed in the terminal), restore.keys (the session kitty.keys saved
# comes back), legacy.keys (no kitty keyboard protocol, 160x44), reload.keys (keymap.json
# changes while it runs), ipc.keys (`bakedit --action ...` and `bakedit file:line` from
# another process reach the window), dump.keys (--dump-input) and probe.keys (--probe),
# writing <out-dir>/<scenario>-<snap>.txt and
# .html for every `snap`, a debug log per scenario, and PNG pictures of a few snapshots
# (headless Chrome with its own profile under <out-dir>).
#
# Nothing of the user's is touched: bakedit's config and state go to <out-dir>/.work, the
# terminal panel runs the login shell with ZDOTDIR there (so no dotfiles are read and no
# history is written), ptyshot strips HERDR_* and the clipboard is in-process
# (BAKEDIT_CLIPBOARD=memory). `--terminal shell` keeps herdr out of it.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"
out="${1:?usage: run.sh <out-dir> [path/to/bakedit]}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
bin="${2:-${CARGO_TARGET_DIR:-$repo/target}/debug/bakedit}"
[ -x "$bin" ] || { echo "no bakedit binary at $bin (cargo build -p bakedit)" >&2; exit 1; }

work="$out/.work"
rm -rf "$work"
mkdir -p "$work/config" "$work/state" "$work/zdotdir" "$work/chrome"
# A plain prompt: the directory and %.
printf "PS1='%%1~ %%# '\nunset HISTFILE\n" > "$work/zdotdir/.zshrc"

# The project: one commit, then one modified file and one new one.
project="$work/demo"
mkdir -p "$project/src"
# None of the user's git configuration (templates, hooks, signing) takes part.
git_() {
    GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
        git -C "$project" -c user.name=Test -c user.email=test@example.invalid -c commit.gpgsign=false -c init.defaultBranch=main "$@"
}
git_ init --quiet
cat > "$project/src/main.rs" <<'RS'
use std::collections::HashMap;

/// Counts how often each word appears.
pub fn word_counts(text: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric());
        if word.is_empty() {
            continue;
        }
        *counts.entry(word.to_lowercase()).or_insert(0) += 1;
    }
    counts
}

fn main() {
    let text = std::fs::read_to_string("notes.txt").expect("readable");
    let counts = word_counts(&text);
    println!("{} different words", counts.len());
}
RS
printf 'pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n' > "$project/src/lib.rs"
printf '# Demo\n\nA small project for bakedit screenshots.\n' > "$project/README.md"
git_ add -A
git_ commit --quiet -m "first"
sed -i '' 's/different words/distinct words/' "$project/src/main.rs"
printf 'notes\n' > "$project/notes.txt"

env_args=(--env "XDG_CONFIG_HOME=$work/config" --env "XDG_STATE_HOME=$work/state" --env "ZDOTDIR=$work/zdotdir")
# run <scenario> <size> [ptyshot options] [-- bakedit options]
run() {
    local name="$1" size="$2"
    shift 2
    local ptyshot_args=() bakedit_args=()
    while [ $# -gt 0 ] && [ "$1" != "--" ]; do ptyshot_args+=("$1"); shift; done
    [ $# -gt 0 ] && shift
    bakedit_args=("$@")
    echo "== $name ($size)"
    uv run --quiet --with pyte python3 "$repo/scripts/ptyshot.py" --size "$size" --out "$out/$name" \
        --script "$here/$name.keys" "${env_args[@]}" ${ptyshot_args[@]+"${ptyshot_args[@]}"} -- \
        "$bin" --terminal shell --new --log "$out/$name.log" ${bakedit_args[@]+"${bakedit_args[@]}"} "$project" > "$out/$name.transcript"
}
run kitty 200x52
# The session kitty.keys left behind comes back.
run restore 200x52
# A fresh start without the kitty keyboard protocol.
run legacy 160x44 --legacy -- --no-restore

# The keymap changes on disk while the editor runs.
( sleep 3; mkdir -p "$work/config/bakedit"
  printf '[{ "context": "Workspace", "bindings": { "cmd-y": "command_palette::Toggle" } }]\n' > "$work/config/bakedit/keymap.json" ) &
run reload 140x36 -- --no-restore
wait
rm -f "$work/config/bakedit/keymap.json"

# Another process hands the running window an action, then a file.
( sleep 3; XDG_STATE_HOME="$work/state" "$bin" --action command_palette::Toggle
  sleep 1; XDG_STATE_HOME="$work/state" "$bin" "$project/README.md:2" ) &
run ipc 160x40 -- --no-restore
wait

# The tools: --dump-input and --probe.
run dump 110x24 -- --dump-input
run probe 110x24 -- --probe

chrome="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
if [ -x "$chrome" ]; then
    shot() {
        local snap="$1" cols="$2" rows="$3"
        # ptyshot draws 14px cells (about 8.4 x 17.5 px) inside 8px of padding.
        local width=$(( cols * 85 / 10 + 24 )) height=$(( rows * 175 / 10 + 24 ))
        "$chrome" --headless=new --disable-gpu --hide-scrollbars --screenshot="$out/$snap.png" \
            --window-size="$width,$height" --force-device-scale-factor=2 \
            --user-data-dir="$work/chrome" "file://$out/$snap.html" > /dev/null 2>&1
        echo "picture: $out/$snap.png"
    }
    shot kitty-start 200 52
    shot kitty-finder 200 52
    shot kitty-editor 200 52
    shot kitty-git 200 52
    shot kitty-chord 200 52
    shot kitty-narrow 120 34
    shot kitty-terminal 200 52
    shot kitty-link 200 52
    shot restore-start 200 52
    shot legacy-start 160 44
    shot legacy-chord 160 44
else
    echo "no Chrome at $chrome: text and HTML snapshots only"
fi
