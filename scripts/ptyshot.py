#!/usr/bin/env python3
"""Run a terminal program in a pty, drive it with keys, and capture what its screen shows.

A development tool for looking at bakedit as a host terminal would show it, without a
terminal window. The pty's output is fed to pyte (a VT emulator); snapshots come out as text
and as HTML that headless Chrome can turn into a picture.

    uv run --with pyte scripts/ptyshot.py --size 160x45 --out /tmp/shots/run \\
        --script keys.txt -- target/debug/bakedit --terminal shell some/dir

Script lines (blank lines and lines starting with # are skipped):
    wait 0.5            pause, in seconds, while still reading output
    text hello world    type plain text
    paste some text     a bracketed paste (\\n in it is a new line)
    key cmd-shift-p     a key with modifiers, pressed and released the way a kitty-protocol
                        terminal sends them (with --legacy: the bytes a legacy terminal sends,
                        and nothing at all for a cmd chord); several keys separated by spaces
                        are pressed one after another
    down ;              press a key and keep it held (kitty protocol only)
    up ;                let a held key go
    send \\x1b[I         raw bytes, with Python escapes
    click 10 5 [cmd]    left click at column 10, row 5 (0-based); "cmd" holds the command key
    click-on [cmd] txt  the same on the first character of the last place the screen shows txt
    text-env text       type text after expanding $NAME from the scenario's environment
    resize 100x30       change the window size (the program gets SIGWINCH)
    snap name           write <out>-name.txt and <out>-name.html
    expect some text    wait (up to --timeout seconds) until the screen shows the text; the
                        run fails if it never does
    expect-not text     the screen must not show the text (after a short read)
    expect-at 3 0 text  the screen shows the text starting at column 3 of row 0
    expect-exit         wait for the program to exit by itself, with status 0
    exec command        run a shell command (sh -c) with the scenario's environment (and
                        PTYSHOT_RAW, the --raw-out file, written so far); the run fails if it
                        exits with an error
    log label           print a line in the transcript (for timings)

A failed expectation writes the screen as <out>-failed.txt/.html, says what was expected on
stderr, and makes ptyshot exit with status 1 once the program has been closed.

The emulated terminal answers the questions bakedit asks at startup as Ghostty would: the
keyboard protocol flags (31) and the device attributes. With --legacy it is a terminal
without the kitty protocol: it answers the device attributes only.
"""
import argparse
import codecs
import fcntl
import html
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import termios
import time

import re

import pyte

MODS = {"shift": 1, "alt": 2, "ctrl": 4, "cmd": 8}
NAMED = {"enter": 13, "escape": 27, "tab": 9, "backspace": 127, "space": 32}
ARROWS = {"up": "A", "down": "B", "right": "C", "left": "D", "home": "H", "end": "F", "f1": "P", "f2": "Q", "f4": "S"}
TILDE = {"insert": 2, "delete": 3, "pageup": 5, "pagedown": 6, "f3": 13, "f5": 15, "f6": 17, "f7": 18, "f8": 19, "f9": 20,
         "f10": 21, "f11": 23, "f12": 24}
SHIFTED = dict(zip('1234567890-=[]\\;\',./`', '!@#$%^&*()_+{}|:"<>?~'))
MODIFIER_KEYS = {"shift": 57441, "ctrl": 57442, "alt": 57443, "cmd": 57444}


def encode_key(spec: str, release: bool = False) -> bytes:
    """`cmd-shift-p` -> the kitty keyboard protocol bytes for that key press (or release)."""
    parts = spec.split("-") if spec != "-" else ["-"]
    if spec.endswith("--"):
        parts = spec[:-2].split("-") + ["-"]
    key = parts[-1]
    mods = sum(MODS[m] for m in parts[:-1])
    event = ":3" if release else ""
    if key in ARROWS:
        return f"\x1b[1;{1 + mods}{event}{ARROWS[key]}".encode()
    if key in TILDE:
        return f"\x1b[{TILDE[key]};{1 + mods}{event}~".encode()
    if key in MODIFIER_KEYS and len(parts) == 1:
        own = MODS[key]
        return f"\x1b[{MODIFIER_KEYS[key]};{1 + (0 if release else own)}{event}u".encode()
    code = NAMED.get(key, ord(key.lower()) if len(key) == 1 else None)
    if code is None:
        raise SystemExit(f"unknown key in script: {spec}")
    if len(key) == 1 and key.isupper():
        mods |= 1
    fields = str(code)
    text = ""
    if len(key) == 1 and mods in (0, 1) and not release:
        # A text key: report the shifted form and the text it types, as flags 4 and 16 ask.
        shifted = key.upper() if key.isalpha() else SHIFTED.get(key, key)
        typed = shifted if mods == 1 else key
        if mods == 1:
            fields += f":{ord(shifted)}"
        text = f";{ord(typed)}"
    elif len(key) == 1 and mods & 1:
        shifted = key.upper() if key.isalpha() else SHIFTED.get(key, key)
        fields += f":{ord(shifted)}"
    return f"\x1b[{fields};{1 + mods}{event}{text}u".encode()


LEGACY_NAMED = {"enter": b"\r", "escape": b"\x1b", "tab": b"\t", "backspace": b"\x7f", "space": b" "}


def encode_key_legacy(spec: str) -> bytes:
    """`ctrl-]`, `a`, `enter`, `up` -> what a terminal without the kitty protocol sends; a cmd
    chord sends nothing there, and alt is an escape prefix."""
    parts = spec.split("-") if spec != "-" else ["-"]
    if spec.endswith("--"):
        parts = spec[:-2].split("-") + ["-"]
    key, mods = parts[-1], set(parts[:-1])
    if "cmd" in mods:
        return b""
    prefix = b"\x1b" if "alt" in mods else b""
    if key in ARROWS:
        return prefix + f"\x1b[{ARROWS[key]}".encode()
    if key in TILDE:
        return prefix + f"\x1b[{TILDE[key]}~".encode()
    if key in LEGACY_NAMED:
        return prefix + LEGACY_NAMED[key]
    if len(key) != 1:
        raise SystemExit(f"unknown key in script: {spec}")
    if "ctrl" in mods:
        code = {"]": 0x1d, "\\": 0x1c, "[": 0x1b, "space": 0x00}.get(key)
        if code is None and key.isalpha():
            code = ord(key.lower()) - ord("a") + 1
        if code is None:
            raise SystemExit(f"no legacy encoding for {spec}")
        return prefix + bytes([code])
    char = key
    if "shift" in mods:
        char = key.upper() if key.isalpha() else SHIFTED.get(key, key)
    return prefix + char.encode()


# Sequences bakedit writes that pyte does not understand (and in one case crashes on): the
# kitty keyboard push/pop/query, private device status reports (colour scheme), XTVERSION,
# window title push/pop, cursor shape. They change nothing pyte draws.
UNSUPPORTED = re.compile(rb"\x1b\[[<>=?][0-9;]*u|\x1b\[\?[0-9;]*n|\x1b\[>[0-9;]*q|\x1b\[[0-9;]*t|\x1b\[[0-9]* q")


def press_and_release(spec: str) -> bytes:
    """A key going down and coming up, as a terminal with event reporting (flag 2) sends it."""
    return encode_key(spec) + encode_key(spec, release=True)


def click(col: int, row: int, cmd: bool) -> bytes:
    out = b""
    if cmd:
        out += encode_key("cmd")
    out += f"\x1b[<0;{col + 1};{row + 1}M\x1b[<0;{col + 1};{row + 1}m".encode()
    if cmd:
        out += encode_key("cmd", release=True)
    return out


def css(color: str, default: str) -> str:
    if color == "default":
        return default
    if len(color) == 6 and all(c in "0123456789abcdefABCDEF" for c in color):
        return "#" + color
    named = {"black": "#12101a", "red": "#ff6363", "green": "#50fa7b", "brown": "#f1fa8c", "blue": "#82aaff",
             "magenta": "#ff00ff", "cyan": "#80ffea", "white": "#f8f8f2"}
    return named.get(color, default)


def to_html(screen: pyte.Screen, fg: str, bg: str) -> str:
    out = ["<!doctype html><meta charset='utf-8'><style>",
           f"body{{margin:0;background:{bg}}}pre{{margin:0;padding:8px;font-family:'MesloLGSDZ Nerd Font Mono','MesloLGS NF',Menlo,monospace;"
           f"font-size:14px;line-height:1.25;color:{fg};background:{bg};font-variant-ligatures:none}}"
           "span{display:inline-block;height:1.25em;vertical-align:top}.cur{outline:1px solid #fff;outline-offset:-1px}",
           f"</style><pre data-size=\"{screen.columns}x{screen.lines}\">"]
    for y in range(screen.lines):
        row = screen.buffer[y]
        x = 0
        while x < screen.columns:
            ch = row[x]
            style = (ch.fg, ch.bg, ch.bold, ch.italics, ch.underscore, ch.reverse)
            is_cursor = (x, y) == (screen.cursor.x, screen.cursor.y) and not screen.cursor.hidden
            text = ""
            while x < screen.columns:
                c = row[x]
                here = (x, y) == (screen.cursor.x, screen.cursor.y) and not screen.cursor.hidden
                if (c.fg, c.bg, c.bold, c.italics, c.underscore, c.reverse) != style or here != is_cursor:
                    break
                text += html.escape(c.data if c.data else "")
                x += 1
                if is_cursor:
                    break
            f, b = css(style[0], fg), css(style[1], bg)
            if style[5]:
                f, b = b, f
            extra = ("font-weight:bold;" if style[2] else "") + ("font-style:italic;" if style[3] else "") + \
                    ("text-decoration:underline;" if style[4] else "")
            cls = " class='cur'" if is_cursor else ""
            out.append(f"<span{cls} style='color:{f};background:{b};{extra}'>{text}</span>")
        out.append("\n")
    out.append("</pre>")
    return "".join(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--size", default="160x45")
    ap.add_argument("--out", required=True, help="path prefix for snapshots")
    ap.add_argument("--script", help="file of script lines; without it, one snapshot named 'final' after --settle seconds")
    ap.add_argument("--settle", type=float, default=1.5)
    ap.add_argument("--legacy", action="store_true", help="do not answer capability queries (a terminal without the kitty protocol)")
    ap.add_argument("--env", action="append", default=[], help="NAME=value for the child")
    ap.add_argument("--timeout", type=float, default=8.0, help="how long `expect` waits, in seconds")
    ap.add_argument("--raw-out", help="write every byte the program wrote to this file")
    ap.add_argument("command", nargs=argparse.REMAINDER)
    args = ap.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        ap.error("no command given")
    cols, rows = (int(n) for n in args.size.lower().split("x"))
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)

    env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor", TERM_PROGRAM="ptyshot", BAKEDIT_CLIPBOARD="memory")
    for name in list(env):
        if name.startswith("HERDR_"):
            del env[name]
    for item in args.env:
        name, _, value = item.partition("=")
        env[name] = value

    pid, fd = pty.fork()
    if pid == 0:
        # The size first, so the program never sees another one at start.
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * 8, rows * 16))
        os.execvpe(command[0], command, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * 8, rows * 16))

    screen = pyte.Screen(cols, rows)
    stream = pyte.ByteStream(screen)
    pending = b""
    answered = False
    raw_file = open(args.raw_out, "wb") if args.raw_out else None
    exit_status = None
    failure = None

    def pump(seconds: float) -> bool:
        """Read output for `seconds`. Returns False once the child has gone."""
        nonlocal pending, answered
        deadline = time.time() + seconds
        while True:
            left = deadline - time.time()
            if left <= 0:
                return True
            ready, _, _ = select.select([fd], [], [], min(left, 0.05))
            if not ready:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                return False
            if not data:
                return False
            if raw_file:
                raw_file.write(data)
            if not answered:
                # Look at everything new (plus a tail of the last read, for a query cut in
                # two): the query may arrive in one read with plenty of output after it.
                window = pending + data
                pending = window[-8:]
                if b"\x1b[c" in window:
                    # Ghostty answers in order: the flags first, the attributes last.
                    os.write(fd, b"\x1b[?62;4c" if args.legacy else b"\x1b[?31u\x1b[?62;4c")
                    answered = True
            stream.feed(UNSUPPORTED.sub(b"", data))

    def snap(name: str) -> None:
        text = "\n".join(line.rstrip() for line in screen.display)
        with open(f"{args.out}-{name}.txt", "w", encoding="utf-8") as f:
            f.write(text + "\n")
        with open(f"{args.out}-{name}.html", "w", encoding="utf-8") as f:
            f.write(to_html(screen, "#f8f8f2", "#12101a"))
        print(f"--- {name} (cursor {screen.cursor.x},{screen.cursor.y}) ---\n{text}")

    def shows(text: str) -> bool:
        return any(text in line for line in screen.display)

    def expect(text: str) -> bool:
        """Read until the screen shows `text`, or the timeout passes."""
        deadline = time.time() + args.timeout
        while True:
            if shows(text):
                return True
            if time.time() > deadline or not pump(0.05):
                return shows(text)

    def wait_exit() -> int | None:
        """Read until the program exits by itself; its exit status, or None at the timeout."""
        deadline = time.time() + args.timeout
        while time.time() < deadline:
            done, status_code = os.waitpid(pid, os.WNOHANG)
            if done:
                return status_code
            pump(0.05)
        return None

    alive = pump(args.settle)
    if args.script:
        # Read the whole script first: it must not change under a run in progress.
        with open(args.script, encoding="utf-8") as script:
            lines = script.read().splitlines()
        for raw in lines:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            verb, _, rest = line.partition(" ")
            if verb == "wait":
                alive = pump(float(rest))
            elif verb == "snap":
                snap(rest or "snap")
            elif verb == "log":
                print(f"--- {rest} at {time.time():.3f}")
            elif verb == "expect":
                started = time.time()
                if not expect(rest):
                    failure = f"the screen never showed {rest!r}"
                    break
                print(f"--- expect {rest!r}: {time.time() - started:.2f}s")
            elif verb == "expect-not":
                pump(0.3)
                if shows(rest):
                    failure = f"the screen shows {rest!r}"
                    break
            elif verb == "expect-at":
                col, row, text = rest.split(" ", 2)
                col, row = int(col), int(row)
                deadline = time.time() + args.timeout
                while screen.display[row][col:col + len(text)] != text and time.time() < deadline and pump(0.05):
                    pass
                if screen.display[row][col:col + len(text)] != text:
                    failure = f"row {row} has {screen.display[row][col:col + len(text)]!r} at column {col}, not {text!r}"
                    break
            elif verb == "exec":
                if raw_file:
                    raw_file.flush()
                result = subprocess.run(["/bin/sh", "-c", rest], env=dict(env, PTYSHOT_RAW=args.raw_out or ""))
                if result.returncode != 0:
                    failure = f"`{rest}` exited with {result.returncode}"
                    break
            elif verb == "expect-exit":
                exit_status = wait_exit()
                if exit_status is None:
                    failure = "the program did not exit"
                    break
                if os.waitstatus_to_exitcode(exit_status) != 0:
                    failure = f"the program exited with {os.waitstatus_to_exitcode(exit_status)}"
                    break
                alive = False
            elif not alive:
                print(f"child exited before: {line}", file=sys.stderr)
                failure = f"the program exited before: {line}"
                break
            elif verb in ("text", "text-env"):
                if verb == "text-env":
                    rest = re.sub(r"\$([A-Za-z_][A-Za-z0-9_]*)", lambda m: env.get(m.group(1), ""), rest)
                if args.legacy:
                    os.write(fd, rest.encode())
                else:
                    os.write(fd, b"".join(press_and_release(c if c != " " else "space") for c in rest))
            elif verb == "paste":
                os.write(fd, b"\x1b[200~" + rest.replace("\\n", "\n").encode() + b"\x1b[201~")
            elif verb == "key":
                for spec in rest.split():
                    os.write(fd, encode_key_legacy(spec) if args.legacy else press_and_release(spec))
                    pump(0.02)
            elif verb == "down":
                os.write(fd, encode_key(rest))
            elif verb == "up":
                os.write(fd, encode_key(rest, release=True))
            elif verb == "resize":
                cols, rows = (int(n) for n in rest.lower().split("x"))
                screen.resize(rows, cols)
                fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * 8, rows * 16))
                os.kill(pid, signal.SIGWINCH)
            elif verb == "send":
                os.write(fd, codecs.decode(rest, "unicode_escape").encode("latin-1"))
            elif verb == "click":
                parts = rest.split()
                # A legacy terminal says nothing about the cmd key.
                os.write(fd, click(int(parts[0]), int(parts[1]), "cmd" in parts[2:] and not args.legacy))
            elif verb == "click-on":
                held = rest.startswith("cmd ")
                wanted = rest[4:] if held else rest
                spot = next(((line.rfind(wanted), y) for y, line in reversed(list(enumerate(screen.display))) if wanted in line), None)
                if spot is None:
                    failure = f"nothing on the screen to click: {wanted!r}"
                    break
                os.write(fd, click(spot[0], spot[1], held and not args.legacy))
            else:
                raise SystemExit(f"unknown script line: {line}")
            if verb not in ("wait", "snap"):
                alive = pump(0.15)
    else:
        snap("final")

    if failure:
        snap("failed")
        print(f"ptyshot: {failure}", file=sys.stderr)

    if exit_status is not None:
        if raw_file:
            raw_file.close()
        return 1 if failure else 0

    # Hang up, and keep reading while the program leaves: closing a terminal waits for its
    # output to be read, so a program that writes on the way out would never finish.
    try:
        os.kill(pid, signal.SIGHUP)
    except ProcessLookupError:
        pass
    deadline = time.time() + 15
    status = None
    while status is None:
        try:
            done, status_code = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            break
        if done:
            status = status_code
            break
        if time.time() > deadline:
            print("the program did not exit after SIGHUP; killing it", file=sys.stderr)
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            return 1
        ready, _, _ = select.select([fd], [], [], 0.05)
        if ready:
            try:
                data = os.read(fd, 65536)
                if raw_file and data:
                    raw_file.write(data)
            except OSError:
                pass
    if raw_file:
        raw_file.close()
    return 1 if failure else 0


if __name__ == "__main__":
    sys.exit(main())
