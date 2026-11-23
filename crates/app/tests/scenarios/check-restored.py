"""Check, from the bytes bakedit wrote (ptyshot --raw-out), that quitting put the terminal
back: after the last frame come the keyboard protocol pop, mouse, focus and paste reporting
off, the cursor shown, and the alternate screen left (after the pop, as the kitty protocol
asks: flags are kept per screen)."""
import sys

data = open(sys.argv[1], "rb").read()
tail = data[data.rfind(b"\x1b[?2026l"):]
wanted = {
    "keyboard protocol popped": b"\x1b[<u",
    "alternate screen left": b"\x1b[?1049l",
    "cursor shown": b"\x1b[?25h",
    "bracketed paste off": b"\x1b[?2004l",
    "focus reports off": b"\x1b[?1004l",
    "SGR mouse off": b"\x1b[?1006l",
}
missing = [what for what, sequence in wanted.items() if sequence not in tail]
if missing:
    sys.exit("not restored: " + ", ".join(missing) + f"\ntail: {tail!r}")
if tail.find(b"\x1b[<u") > tail.find(b"\x1b[?1049l"):
    sys.exit("the keyboard protocol was popped after leaving the alternate screen")
print("terminal restored")
