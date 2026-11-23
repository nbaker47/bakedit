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
    key cmd-shift-p     a key with modifiers, sent the way a kitty-protocol terminal sends it
    send \\x1b[I         raw bytes, with Python escapes
    click 10 5 [cmd]    left click at column 10, row 5 (0-based); "cmd" holds the command key
    snap name           write <out>-name.txt and <out>-name.html

The emulated terminal answers the two questions bakedit asks at startup (keyboard protocol
flags and device attributes) as Ghostty would, unless --legacy is given.
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
import sys
import termios
import time

import pyte

MODS = {"shift": 1, "alt": 2, "ctrl": 4, "cmd": 8}
NAMED = {"enter": 13, "escape": 27, "tab": 9, "backspace": 127, "space": 32}
ARROWS = {"up": "A", "down": "B", "right": "C", "left": "D", "home": "H", "end": "F"}
TILDE = {"insert": 2, "delete": 3, "pageup": 5, "pagedown": 6}
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
           "</style><pre>"]
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
    ap.add_argument("command", nargs=argparse.REMAINDER)
    args = ap.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        ap.error("no command given")
    cols, rows = (int(n) for n in args.size.lower().split("x"))
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)

    pid, fd = pty.fork()
    if pid == 0:
        env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor", BAKEDIT_CLIPBOARD="memory")
        for name in list(env):
            if name.startswith("HERDR_"):
                del env[name]
        for item in args.env:
            name, _, value = item.partition("=")
            env[name] = value
        os.execvpe(command[0], command, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * 8, rows * 16))

    screen = pyte.Screen(cols, rows)
    stream = pyte.ByteStream(screen)
    pending = b""
    answered = args.legacy

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
            if not answered:
                pending = (pending + data)[-64:]
                if b"\x1b[?u" in pending or b"\x1b[c" in pending:
                    os.write(fd, b"\x1b[?31u\x1b[?62;4c")
                    answered = True
            stream.feed(data)

    def snap(name: str) -> None:
        text = "\n".join(line.rstrip() for line in screen.display)
        with open(f"{args.out}-{name}.txt", "w", encoding="utf-8") as f:
            f.write(text + "\n")
        with open(f"{args.out}-{name}.html", "w", encoding="utf-8") as f:
            f.write(to_html(screen, "#f8f8f2", "#12101a"))
        print(f"--- {name} (cursor {screen.cursor.x},{screen.cursor.y}) ---\n{text}")

    alive = pump(args.settle)
    if args.script:
        for raw in open(args.script, encoding="utf-8"):
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            verb, _, rest = line.partition(" ")
            if verb == "wait":
                alive = pump(float(rest))
            elif verb == "snap":
                snap(rest or "snap")
            elif not alive:
                print(f"child exited before: {line}", file=sys.stderr)
                break
            elif verb == "text":
                os.write(fd, b"".join(encode_key(c) if c != " " else encode_key("space") for c in rest))
            elif verb == "key":
                os.write(fd, encode_key(rest))
            elif verb == "send":
                os.write(fd, codecs.decode(rest, "unicode_escape").encode("latin-1"))
            elif verb == "click":
                parts = rest.split()
                os.write(fd, click(int(parts[0]), int(parts[1]), "cmd" in parts[2:]))
            else:
                raise SystemExit(f"unknown script line: {line}")
            if verb not in ("wait", "snap"):
                alive = pump(0.15)
    else:
        snap("final")

    try:
        os.kill(pid, signal.SIGHUP)
    except ProcessLookupError:
        pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
