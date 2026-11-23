#!/usr/bin/env bash
# Drive the real bakedit binary through scripts/ptyshot.py, check what its screen shows and
# keep pictures of it.
#
#   crates/app/tests/scenarios/run.sh <out-dir> [path/to/bakedit] [scenario ...]
#
# Every <scenario>.keys file here is a ptyshot script; its `expect` lines are the checks.
# With no scenario names, all of them run, in the order below. Each one writes
# <out-dir>/<scenario>-<snap>.txt and .html for every `snap`, a transcript and a debug log,
# and PASS or FAIL with the reason. The exit status is 1 if any scenario failed.
# BAKEDIT_SCENARIO_PICTURES=0 skips the PNG pictures (headless Chrome, its own profile).
#
# Nothing of the user's is touched. Every bakedit here gets --here, so it runs in ptyshot's
# pty and never opens a Ghostty window. bakedit's config and state go to <out-dir>/.work, with
# zed-settings.json (Nathan's editor settings) where bakedit looks for Zed's. The terminal
# panel runs zsh with ZDOTDIR there (no dotfiles read, no history written) or plain sh.
# ptyshot strips HERDR_* and keeps the clipboard in the process (BAKEDIT_CLIPBOARD=memory).
# `--terminal shell` keeps herdr out of every scenario but herdr-session, which starts its
# own throwaway herdr server (crates/herdr/tests/isolated-session.sh) with a private config,
# home and socket, points the terminal.command setting at it, and stops it again.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"
out="${1:?usage: run.sh <out-dir> [path/to/bakedit] [scenario ...]}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
bin="${2:-${CARGO_TARGET_DIR:-$repo/target}/debug/bakedit}"
[ -x "$bin" ] || { echo "no bakedit binary at $bin (cargo build -p bakedit)" >&2; exit 1; }
shift $(( $# >= 2 ? 2 : $# ))
all=(kitty restore panels editing modes search git tree links navlayer herdr bigfile narrow legacy reload ipc quit dump probe herdr-session herdr-readme)
wanted=("$@")
[ ${#wanted[@]} -eq 0 ] && wanted=("${all[@]}")

work="$out/.work"
rm -rf "$work"
mkdir -p "$work/config/zed" "$work/state" "$work/zdotdir" "$work/chrome"
cp "$here/zed-settings.json" "$work/config/zed/settings.json"
# A plain prompt: the directory and %.
printf "PS1='%%1~ %%# '\nunset HISTFILE\n" > "$work/zdotdir/.zshrc"
project="$work/demo"

# None of the user's git configuration (templates, hooks, signing) takes part.
git_() {
    GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null \
        git -C "$project" -c user.name=Test -c user.email=test@example.invalid -c commit.gpgsign=false -c init.defaultBranch=main "$@"
}

# The project: one commit, then one changed file and one new one. Made afresh for each
# scenario that changes it.
make_project() {
    rm -rf "$project"
    mkdir -p "$project/src" "$project/data"
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
    printf '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2021"\n' > "$project/Cargo.toml"
    # 50,000 lines of Rust, for the scenario that checks the editor keeps up.
    awk 'BEGIN { for (i = 1; i <= 50000; i++) printf "fn f%d(x: u32) -> u32 { x * %d + 1 } // line %d\n", i, i % 97, i }' > "$project/data/big.rs"
    git_ init --quiet
    git_ add -A
    git_ commit --quiet -m "first"
    sed -i '' 's/different words/distinct words/' "$project/src/main.rs"
    printf 'notes\n' > "$project/notes.txt"
}

env_args=(--env "XDG_CONFIG_HOME=$work/config" --env "XDG_STATE_HOME=$work/state" --env "ZDOTDIR=$work/zdotdir"
          --env "BAKEDIT_SCENARIO_PROJECT=$project" --env "BAKEDIT_SCENARIO_WORK=$work" --env "BAKEDIT_SCENARIO_HERE=$here")
failed=()
passed=()

# run <scenario> <size> [ptyshot options] [-- bakedit options]
run() {
    local name="$1" size="$2"
    shift 2
    local ptyshot_args=() bakedit_args=()
    while [ $# -gt 0 ] && [ "$1" != "--" ]; do ptyshot_args+=("$1"); shift; done
    [ $# -gt 0 ] && shift
    bakedit_args=("$@")
    local started=$SECONDS
    rm -f "$out/$name"-*.txt "$out/$name"-*.html "$out/$name"-*.png
    if uv run --quiet --with pyte python3 "$repo/scripts/ptyshot.py" --size "$size" --out "$out/$name" \
        --script "$here/$name.keys" "${env_args[@]}" ${ptyshot_args[@]+"${ptyshot_args[@]}"} -- \
        "$bin" --here --log "$out/$name.log" ${bakedit_args[@]+"${bakedit_args[@]}"} > "$out/$name.transcript" 2> "$out/$name.err"; then
        echo "PASS $name ($(( SECONDS - started ))s)"
        passed+=("$name")
    else
        echo "FAIL $name: $(tail -1 "$out/$name.err")"
        failed+=("$name")
    fi
}

wants() {
    local name
    for name in "${wanted[@]}"; do [ "$name" = "$1" ] && return 0; done
    return 1
}

make_project
for scenario in "${all[@]}"; do
    wants "$scenario" || continue
    case "$scenario" in
        kitty) run kitty 200x52 -- --terminal shell --new "$project" ;;
        # The session kitty.keys left behind comes back.
        restore) run restore 200x52 -- --terminal shell --new "$project" ;;
        panels) make_project; run panels 200x52 -- --terminal shell --new --no-restore "$project" ;;
        editing) make_project; run editing 200x52 -- --terminal shell --new --no-restore "$project" ;;
        modes) make_project; run modes 200x52 -- --terminal shell --new --no-restore "$project" ;;
        search) make_project; run search 200x52 -- --terminal shell --new --no-restore "$project" ;;
        git) make_project; run git 200x52 -- --terminal shell --new --no-restore "$project" ;;
        tree) make_project; run tree 200x52 -- --terminal shell --new --no-restore "$project" ;;
        links) make_project; run links 200x52 -- --terminal shell --new --no-restore "$project" ;;
        navlayer) make_project; run navlayer 200x52 -- --terminal shell --new --no-restore "$project" ;;
        herdr) make_project; run herdr 200x52 -- --terminal /bin/sh --new --no-restore "$project" ;;
        bigfile) make_project; run bigfile 200x52 --timeout 4 -- --terminal shell --new --no-restore "$project" ;;
        narrow) make_project; run narrow 100x30 -- --terminal shell --new --no-restore "$project" ;;
        # A fresh start without the kitty keyboard protocol.
        legacy) make_project; run legacy 160x44 --legacy -- --terminal shell --new --no-restore "$project" ;;
        reload)
            # The keymap changes on disk while the editor runs.
            ( sleep 3; mkdir -p "$work/config/bakedit"
              printf '[{ "context": "Workspace", "bindings": { "cmd-y": "command_palette::Toggle" } }]\n' > "$work/config/bakedit/keymap.json" ) &
            run reload 140x36 -- --terminal shell --new --no-restore "$project"
            wait
            rm -f "$work/config/bakedit/keymap.json" ;;
        ipc)
            # Another process hands the running window an action, then a file.
            ( sleep 3; XDG_STATE_HOME="$work/state" "$bin" --action command_palette::Toggle
              sleep 1; XDG_STATE_HOME="$work/state" "$bin" --here "$project/README.md:2" ) &
            run ipc 160x40 -- --terminal shell --new --no-restore "$project"
            wait ;;
        quit) run quit 120x30 --raw-out "$out/quit.raw" -- --terminal shell --new --no-restore "$project" ;;
        # The tools: --dump-input and --probe.
        dump) run dump 110x24 -- --terminal shell --dump-input ;;
        probe) run probe 110x24 -- --terminal shell --probe ;;
        herdr-session|herdr-readme)
            if ! command -v herdr > /dev/null; then
                echo "SKIP $scenario: herdr is not installed"
                continue
            fi
            make_project
            # A herdr server of our own: fresh config, home and socket, nothing inherited.
            exports="$("$repo/crates/herdr/tests/isolated-session.sh" start)" || { echo "FAIL $scenario: no test server"; failed+=("$scenario"); continue; }
            eval "$exports"
            session="$(cat "$BAKEDIT_TEST_HERDR_DIR/session-name")"
            saved_env=("${env_args[@]}")
            # bakedit and the herdr client in its panel see only the test server's folders.
            env_args+=(--env "HOME=$BAKEDIT_TEST_HERDR_DIR/home" --env "XDG_CONFIG_HOME=$BAKEDIT_TEST_HERDR_DIR"
                       --env "BAKEDIT_SCENARIO_HERDR_SOCKET=$BAKEDIT_TEST_HERDR_SOCKET" --env "SHELL=/bin/sh")
            # What the terminal runs comes from the terminal.command setting (herdr unless set),
            # here the test server's session.
            mkdir -p "$BAKEDIT_TEST_HERDR_DIR/bakedit"
            printf '{ "terminal": { "dock": "left", "command": "herdr --session %s" }, "project_panel": { "dock": "right" } }\n' \
                "$session" > "$BAKEDIT_TEST_HERDR_DIR/bakedit/settings.json"
            run "$scenario" 200x52 -- --new --no-restore "$project"
            env_args=("${saved_env[@]}")
            "$repo/crates/herdr/tests/isolated-session.sh" stop "$BAKEDIT_TEST_HERDR_DIR" > /dev/null ;;
    esac
done

chrome="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
if [ "${BAKEDIT_SCENARIO_PICTURES:-1}" != "0" ] && [ -x "$chrome" ]; then
    for html in "$out"/*.html; do
        [ -e "$html" ] || continue
        snap="$(basename "$html" .html)"
        # The screen size ptyshot recorded on the <pre>.
        size="$(grep -o -m1 'data-size="[0-9]*x[0-9]*"' "$html" | grep -o '[0-9]*x[0-9]*')"
        [ -n "$size" ] || continue
        cols="${size%x*}" rows="${size#*x}"
        # ptyshot draws 14px cells (about 8.4 x 17.5 px) inside 8px of padding.
        width=$(( cols * 85 / 10 + 24 )) height=$(( rows * 175 / 10 + 24 ))
        "$chrome" --headless=new --disable-gpu --hide-scrollbars --screenshot="$out/$snap.png" \
            --window-size="$width,$height" --force-device-scale-factor=2 \
            --user-data-dir="$work/chrome" "file://$html" > /dev/null 2>&1
    done
    echo "pictures: $out/*.png"
fi

echo "${#passed[@]} passed, ${#failed[@]} failed${failed[*]:+: ${failed[*]}}"
[ ${#failed[@]} -eq 0 ]
