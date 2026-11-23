#!/bin/sh
# Start or stop a throwaway herdr server for the tests in isolated_session.rs.
#
#   eval "$(crates/herdr/tests/isolated-session.sh start)"
#   cargo test -p bakedit-herdr --test isolated_session -- --nocapture
#   crates/herdr/tests/isolated-session.sh stop "$BAKEDIT_TEST_HERDR_DIR"
#
# `start` prints two export lines: BAKEDIT_TEST_HERDR_SOCKET, which the tests look for, and
# BAKEDIT_TEST_HERDR_DIR, which `stop` needs. `run` does the same in the foreground, for a
# supervisor that wants to own the process: it prints the lines and then becomes the server.
#
# The server shares nothing with a herdr the user may be working in. Its environment is
# built from scratch, so none of the HERDR_* variables of an enclosing herdr pane reach it;
# its config, state, cache and home directories are fresh ones under $TMPDIR, so it loads
# none of the user's plugins or key bindings and cannot touch their collapse state file; and
# its config turns off update checks and sounds. Panes run /bin/sh without the user's
# dotfiles.
#
# The directory name is short on purpose: a unix socket path must fit in about 100 bytes.

set -eu

herdr_bin=${HERDR_BIN:-$(command -v herdr || true)}

usage() {
    echo "usage: $0 start | run | stop <dir>" >&2
    exit 2
}

fail() {
    echo "isolated-session: $*" >&2
    exit 1
}

prepare() {
    [ -n "$herdr_bin" ] && [ -x "$herdr_bin" ] || fail "herdr is not installed (set HERDR_BIN)"
    tmp=${TMPDIR:-/tmp}
    dir=$(mktemp -d "${tmp%/}/bh.XXXXXX")
    name="bkt-$$"
    socket="$dir/herdr/sessions/$name/herdr.sock"
    # herdr also binds herdr-client.sock beside it, seven characters longer; macOS allows 103.
    [ ${#socket} -le 96 ] || { rm -rf "$dir"; fail "socket path too long for a unix socket: $socket"; }
    mkdir -p "$dir/herdr" "$dir/state" "$dir/cache" "$dir/data" "$dir/home"
    cat > "$dir/herdr/config.toml" <<'EOF'
# Throwaway config for the bakedit-herdr integration tests.
onboarding = false

[update]
version_check = false
manifest_check = false

[terminal]
default_shell = "/bin/sh"
shell_mode = "non_login"

[ui.sound]
enabled = false

[ui.toast]
delivery = "off"
EOF
    echo "$name" > "$dir/session-name"
}

# The server with nothing inherited from this shell, not even its working directory.
serve() {
    cd "$dir/home"
    exec env -i \
        PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
        HOME="$dir/home" \
        TMPDIR="${TMPDIR:-/tmp}" \
        TERM="xterm-256color" \
        LANG="en_US.UTF-8" \
        SHELL="/bin/sh" \
        XDG_CONFIG_HOME="$dir" \
        XDG_STATE_HOME="$dir/state" \
        XDG_CACHE_HOME="$dir/cache" \
        XDG_DATA_HOME="$dir/data" \
        "$herdr_bin" --session "$name" server
}

print_exports() {
    echo "export BAKEDIT_TEST_HERDR_SOCKET='$socket'"
    echo "export BAKEDIT_TEST_HERDR_DIR='$dir'"
}

start() {
    prepare
    (serve > "$dir/server.out" 2>&1 < /dev/null) &
    echo $! > "$dir/server.pid"
    tries=0
    while [ ! -S "$socket" ]; do
        tries=$((tries + 1))
        if [ "$tries" -gt 100 ]; then
            cat "$dir/server.out" >&2 || true
            fail "the server did not create $socket"
        fi
        sleep 0.1
    done
    print_exports
}

run() {
    prepare
    echo $$ > "$dir/server.pid"
    print_exports
    serve
}

stop() {
    dir=${1:-}
    [ -n "$dir" ] && [ -f "$dir/server.pid" ] && [ -f "$dir/session-name" ] || fail "not a test session directory: $dir"
    case "$dir" in
        */bh.*) ;;
        *) fail "refusing to touch $dir" ;;
    esac
    pid=$(cat "$dir/server.pid")
    name=$(cat "$dir/session-name")
    # Only ever signal the server this script started: the pid must still be that process.
    if ps -o command= -p "$pid" 2>/dev/null | grep -q -- "--session $name server"; then
        kill "$pid"
        tries=0
        while kill -0 "$pid" 2>/dev/null && [ "$tries" -lt 50 ]; do
            tries=$((tries + 1))
            sleep 0.1
        done
    fi
    rm -rf "$dir"
    echo "stopped $name and removed $dir"
}

case "${1:-}" in
    start) start ;;
    run) run ;;
    stop) stop "${2:-}" ;;
    *) usage ;;
esac
