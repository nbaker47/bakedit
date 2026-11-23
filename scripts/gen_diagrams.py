#!/usr/bin/env python3
"""Draws the README diagrams (docs/img/*.svg). Run: python3 scripts/gen_diagrams.py

The nav-layer mapping mirrors NavLayerConfig::default() in crates/input/src/nav.rs.
Each SVG carries its own dark background so it reads the same on GitHub light and dark.
"""
import pathlib

OUT = pathlib.Path(__file__).resolve().parent.parent / "docs" / "img"
OUT.mkdir(parents=True, exist_ok=True)

BG, PANEL, KEY, KEYLINE = "#11111b", "#181825", "#1e1e2e", "#313244"
TEXT, DIM = "#cdd6f4", "#7f849c"
BLUE, GREEN, PEACH, MAUVE, RED, YELLOW = "#89b4fa", "#a6e3a1", "#fab387", "#cba6f7", "#f38ba8", "#f9e2af"
FONT = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
SANS = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"


def svg(w, h, body, title):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" '
        f'role="img" aria-label="{title}">\n<title>{title}</title>\n'
        f'<rect width="{w}" height="{h}" rx="14" fill="{BG}"/>\n{body}</svg>\n'
    )


def text(x, y, s, size=14, fill=TEXT, anchor="middle", weight="400", font=SANS):
    return (f'<text x="{x}" y="{y}" font-family="{font}" font-size="{size}" fill="{fill}" '
            f'text-anchor="{anchor}" font-weight="{weight}">{s}</text>\n')


def rect(x, y, w, h, fill, stroke=None, r=8, sw=1.5):
    s = f' stroke="{stroke}" stroke-width="{sw}"' if stroke else ""
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}"{s}/>\n'


# ---------------------------------------------------------------- nav layer keyboard
ROWS = [
    (0, "qwertyuiop"),
    (14, "asdfghjkl;"),
    (36, "zxcvbnm,./"),
]
# key -> (big label, small label, colour)
NAV = {
    "e": ("↑", "up", BLUE), "s": ("←", "left", BLUE), "d": ("↓", "down", BLUE), "f": ("→", "right", BLUE),
    "j": ("shift", "", PEACH), "k": ("cmd", "", PEACH), "l": ("alt", "", PEACH), "h": ("ctrl", "", PEACH),
    "a": ("tab", "", MAUVE), "c": ("esc", "", MAUVE),
    ";": ("HOLD", "layer key", GREEN),
}


def keyboard(layer, x0=40, y0=96, k=64, gap=8):
    out = ""
    for ry, (indent, row) in enumerate(ROWS):
        for i, ch in enumerate(row):
            x = x0 + indent + i * (k + gap)
            y = y0 + ry * (k + gap)
            if layer and ch in NAV:
                big, small, col = NAV[ch]
                out += rect(x, y, k, k, KEY, col, sw=2.5)
                out += text(x + 9, y + 18, ch, 12, DIM, "start", font=FONT)
                size = 26 if len(big) == 1 else 17
                out += text(x + k / 2, y + 43 if small else y + 45, big, size, col, weight="700", font=FONT)
                if small:
                    out += text(x + k / 2, y + 58, small, 11, DIM)
            else:
                out += rect(x, y, k, k, KEY, KEYLINE)
                out += text(x + k / 2, y + 41, ch, 22, DIM if layer else TEXT, font=FONT)
    return out


def nav_layer():
    w, h = 830, 400
    b = text(w / 2, 40, "The nav layer", 24, TEXT, weight="700")
    b += text(w / 2, 64, "hold ; with your right pinky and the home row turns into arrows and modifiers", 14, DIM)
    b += keyboard(True)
    # hand labels
    b += text(40, 330, "left hand", 13, DIM, "start")
    b += text(40, 350, "e s d f  =  up, left, down, right (an inverted T under the left hand)", 13, BLUE, "start")
    b += text(40, 372, "a  =  tab      c  =  esc", 13, MAUVE, "start")
    b += text(w - 40, 330, "right hand", 13, DIM, "end")
    b += text(w - 40, 350, "h j k l  =  ctrl, shift, cmd, alt (stack them freely)", 13, PEACH, "end")
    b += text(w - 40, 372, "a quick tap of ;  still types ;", 13, GREEN, "end")
    return svg(w, h, b, "The nav layer: hold semicolon and e s d f become arrows, j k l h become shift, cmd, alt and ctrl")


# ---------------------------------------------------------------- combos
def chord(x, y, keys, result, note, col):
    out = ""
    cx = x
    for i, (label, c) in enumerate(keys):
        if i:
            out += text(cx + 9, y + 26, "+", 18, DIM)
            cx += 22
        wd = 36 + 9 * len(label)
        out += rect(cx, y, wd, 40, KEY, c, sw=2)
        out += text(cx + wd / 2, y + 26, label, 15, c, weight="700", font=FONT)
        cx += wd
    out += text(x + 322, y + 27, "→", 20, DIM, "start")
    out += text(x + 356, y + 18, result, 16, TEXT, "start", weight="600", font=FONT)
    out += text(x + 356, y + 36, note, 12.5, DIM, "start")
    return out


def combos():
    w, h = 830, 480
    b = text(w / 2, 40, "What the layer lets you do without leaving the home row", 22, TEXT, weight="700")
    b += text(w / 2, 64, "every chord starts with ;  held down", 14, DIM)
    rows = [
        ([(";", GREEN), ("s", BLUE)], "←", "move left one character"),
        ([(";", GREEN), ("j", PEACH), ("f", BLUE)], "shift-→", "extend the selection right"),
        ([(";", GREEN), ("l", PEACH), ("s", BLUE)], "alt-←", "jump back a word"),
        ([(";", GREEN), ("k", PEACH), ("j", PEACH), ("e", BLUE)], "cmd-shift-↑", "select to the top of the file"),
        ([(";", GREEN), ("k", PEACH), ("p", TEXT)], "cmd-p", "open the file finder, nothing else to reach for"),
        ([(";", GREEN), ("k", PEACH), ("l", PEACH), ("e", BLUE)], "cmd-alt-↑", "move focus to the pane above (herdr aware)"),
        ([(";", GREEN), ("c", MAUVE)], "esc", "back to nav mode"),
    ]
    y = 96
    for keys, result, note in rows:
        b += chord(40, y, keys, result, note, GREEN)
        y += 54
    return svg(w, h, b, "Example nav-layer chords: arrows, selection, word jumps and shortcuts from the home row")


# ---------------------------------------------------------------- layout
def layout():
    w, h = 830, 400
    b = text(w / 2, 40, "The window", 24, TEXT, weight="700")
    b += text(w / 2, 64, "three columns, one status bar, every panel a keystroke away", 14, DIM)
    top, bot = 90, 318
    cols = [
        (40, 230, "Terminal", "runs herdr", "cmd-t", GREEN, [("Git", "cmd-g", PEACH)]),
        (284, 290, "Editor", "tabs · find · minimap · git gutter", "cmd-e", BLUE, []),
        (588, 202, "File tree", "icons · rename · new", "cmd-shift-e", MAUVE, []),
    ]
    for x, wd, name, sub, key, col, extra in cols:
        b += rect(x, top, wd, bot - top, PANEL, col, sw=2)
        if extra:
            split = top + 112
            b = b.replace(rect(x, top, wd, bot - top, PANEL, col, sw=2), rect(x, top, wd, split - top - 6, PANEL, col, sw=2))
            b += text(x + wd / 2, top + 38, name, 18, col, weight="700")
            b += text(x + wd / 2, top + 58, sub, 12, DIM)
            b += text(x + wd / 2, top + 80, key, 13, col, font=FONT)
            for n2, k2, c2 in extra:
                b += rect(x, split, wd, bot - split, PANEL, c2, sw=2)
                b += text(x + wd / 2, split + 38, n2, 18, c2, weight="700")
                b += text(x + wd / 2, split + 58, "stage · diff · commit", 12, DIM)
                b += text(x + wd / 2, split + 80, k2, 13, c2, font=FONT)
        else:
            b += text(x + wd / 2, top + 100, name, 18, col, weight="700")
            b += text(x + wd / 2, top + 122, sub, 12, DIM)
            b += text(x + wd / 2, top + 148, key, 13, col, font=FONT)
    b += rect(40, 332, 750, 36, KEY, KEYLINE)
    b += text(54, 355, "NAV", 13, BG, "start", weight="700", font=FONT)
    b = b.replace(text(54, 355, "NAV", 13, BG, "start", weight="700", font=FONT),
                  rect(50, 339, 44, 22, BLUE, None, 4) + text(72, 355, "NAV", 12, BG, "middle", weight="700", font=FONT))
    b += text(106, 355, "branch  ±changes", 12.5, DIM, "start", font=FONT)
    b += text(w - 54, 355, "2:1   Rust   Spaces: 4   LF   ;layer", 12.5, DIM, "end", font=FONT)
    b += text(w / 2, 390, "cmd-alt-f hides every dock for a full-screen editor", 12.5, DIM)
    return svg(w, h, b, "bakedit window layout: terminal and git on the left, editor in the middle, file tree on the right")


# ---------------------------------------------------------------- modes
def modes():
    w, h = 830, 300
    b = text(w / 2, 40, "Two modes, no vim keys to memorise", 24, TEXT, weight="700")
    b += text(w / 2, 64, "NAV moves and acts, EDIT types. The nav layer works in both.", 14, DIM)
    b += rect(70, 100, 250, 110, PANEL, BLUE, sw=2)
    b += text(195, 135, "NAV", 22, BLUE, weight="700", font=FONT)
    b += text(195, 162, "arrows, cmd-keys", 13, TEXT, font=FONT)
    b += text(195, 184, "letters type nothing", 13, TEXT, font=FONT)
    b += rect(510, 100, 250, 110, PANEL, GREEN, sw=2)
    b += text(635, 135, "EDIT", 22, GREEN, weight="700", font=FONT)
    b += text(635, 162, "type, paste, cmd-keys", 13, TEXT, font=FONT)
    b += text(635, 184, "all Zed shortcuts still work", 13, TEXT, font=FONT)
    arrow = f'stroke="{DIM}" stroke-width="2" fill="none" marker-end="url(#a)"'
    b = ('<defs><marker id="a" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" '
         f'orient="auto-start-reverse"><path d="M0 0L10 5L0 10z" fill="{DIM}"/></marker></defs>\n') + b
    b += f'<path d="M320 134 H510" {arrow}/>\n'
    b += text(415, 124, "cmd-i", 13, GREEN, font=FONT)
    b += f'<path d="M510 180 H320" {arrow}/>\n'
    b += text(415, 202, "cmd-i   or   esc", 13, BLUE, font=FONT)
    b += rect(205, 236, 420, 40, KEY, KEYLINE)
    b += text(415, 261, "hold ;  →  arrows + modifiers, in either mode", 13, YELLOW, font=FONT)
    return svg(w, h, b, "Mode diagram: NAV and EDIT, with the nav layer available in both")


for name, fn in [("nav-layer", nav_layer), ("nav-combos", combos), ("layout", layout), ("modes", modes)]:
    (OUT / f"{name}.svg").write_text(fn())
    print("wrote", OUT / f"{name}.svg")
