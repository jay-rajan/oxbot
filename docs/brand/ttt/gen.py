"""Generate oxbot's tic-tac-toe brand assets (O and X) from one set of shapes.

Usage: python3 -I gen.py <repo>
Writes docs/brand/ttt/*.svg and docs/brand/ttt/strings.json (inline strings for the UI).
"""
import json
import re
import sys
from pathlib import Path

REPO = Path(sys.argv[1])
OUT = REPO / "docs/brand/ttt"
OUT.mkdir(parents=True, exist_ok=True)

INK = "#111111"      # icon ground
O_WHITE = "#FFFFFF"
X_RED = "#FF3737"     # same as the UI's --rust
DIVIDER = "#3A3A40"


def ring(cx, cy, r, w, color, extra=""):
    return f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="none" stroke="{color}" stroke-width="{w}"{extra}/>'


def cross(cx, cy, h, w, color, extra=""):
    d = f"M{cx-h} {cy-h}L{cx+h} {cy+h}M{cx+h} {cy-h}L{cx-h} {cy+h}"
    return f'<path d="{d}" fill="none" stroke="{color}" stroke-width="{w}" stroke-linecap="round"{extra}/>'


# ---------- app icon (1024, macOS-style rounded square, like the old one)
def icon(ground=True):
    bg = f'<rect width="1024" height="1024" rx="230" fill="{INK}"/>' if ground else ""
    divider = f'<line x1="512" y1="356" x2="512" y2="668" stroke="{DIVIDER}" stroke-width="22" stroke-linecap="round"/>'
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" role="img" aria-label="oxbot">'
            f'{bg}{divider}{ring(300, 512, 135, 86, O_WHITE)}{cross(724, 512, 124, 90, X_RED)}</svg>')


# Android adaptive foreground: 108dp canvas, marks inside the 66dp safe zone.
def android_fg():
    inner = icon(False).split(">", 1)[1].rsplit("</svg>", 1)[0]
    scale = 0.62
    t = (1024 - 1024 * scale) / 2
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">'
            f'<g transform="translate({t} {t}) scale({scale})">{inner}</g></svg>')


# ---------- menu-bar (tray) icon: macOS template image, black on clear, 22pt
def tray():
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 22 22">'
            f'{ring(6.6, 11, 4.0, 2.3, "#000")}{cross(15.6, 11, 3.6, 2.5, "#000")}</svg>')


# ---------- wordmark: O ring + red X + "bot" (glyphs reused from the old wordmark)
def bot_glyphs(index_html):
    m = re.search(r'd="M836\.4 715\.4.*?Z (M1172\.9.*?)"/>', index_html, re.S)
    if not m:
        sys.exit("couldn't find the old wordmark's glyphs")
    return m.group(1)


def wordmark(glyphs, ui=False):
    # x-height of the old glyphs: 642..872 (230), baseline 868.
    cy = 757
    o = ring(115, cy, 88, 54, "currentColor")
    xcls = ' class="wm-red"' if ui else ""
    x = f'<g{xcls}>{cross(367, cy, 80, 54, X_RED)}</g>'
    bot = (f'<path transform="translate(-466.2 0)" fill="currentColor" stroke="currentColor" '
           f'stroke-width="12" stroke-linejoin="round" d="{glyphs}"/>')
    cls = ' class="wordmark"' if ui else ""
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="-8 540 1196 345" role="img" aria-label="oxbot"{cls}>'
            f'{o}{x}{bot}</svg>')


# ---------- bot avatars: 6 O pieces and 6 X pieces, each with a face. "{C}" is the colour.
W = "#FFFFFF"
DARK = "#111111"


def eyes(style, l, r):
    (lx, ly), (rx, ry) = l, r
    if style == "dots":
        return f'<circle cx="{lx}" cy="{ly}" r="4.4" fill="{W}"/><circle cx="{rx}" cy="{ry}" r="4.4" fill="{W}"/>'
    if style == "tall":
        return f'<ellipse cx="{lx}" cy="{ly}" rx="3.4" ry="5.6" fill="{W}"/><ellipse cx="{rx}" cy="{ry}" rx="3.4" ry="5.6" fill="{W}"/>'
    if style == "happy":
        return (f'<path d="M{lx-4.5} {ly+2} Q{lx} {ly-4.5} {lx+4.5} {ly+2}M{rx-4.5} {ry+2} Q{rx} {ry-4.5} {rx+4.5} {ry+2}" '
                f'fill="none" stroke="{W}" stroke-width="3.2" stroke-linecap="round"/>')
    if style == "pupils":
        return (f'<circle cx="{lx}" cy="{ly}" r="5.8" fill="{W}"/><circle cx="{rx}" cy="{ry}" r="5.8" fill="{W}"/>'
                f'<circle cx="{lx+1.6}" cy="{ly}" r="2.7" fill="{DARK}"/><circle cx="{rx+1.6}" cy="{ry}" r="2.7" fill="{DARK}"/>')
    if style == "sleepy":
        return (f'<path d="M{lx-4.5} {ly} H{lx+4.5}M{rx-4.5} {ry} H{rx+4.5}" fill="none" stroke="{W}" '
                f'stroke-width="3.2" stroke-linecap="round"/>')
    if style == "wink":
        return (f'<circle cx="{lx}" cy="{ly}" r="4.4" fill="{W}"/>'
                f'<path d="M{rx-4.5} {ry+1.5} Q{rx} {ry-4} {rx+4.5} {ry+1.5}" fill="none" stroke="{W}" stroke-width="3.2" stroke-linecap="round"/>')
    raise ValueError(style)


STYLES = ["dots", "tall", "happy", "pupils", "sleepy", "wink"]
HEAD = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 3 100 94" aria-hidden="true">'


def o_avatar(style):
    # A thick ring; the face sits on the top of the ring's band.
    body = f'<g class="pc pc-o">{ring(50, 54, 28, 20, "{C}")}'
    face = eyes(style, (37.5, 33), (62.5, 33))
    return f'{HEAD}{body}{face}</g></svg>'


def x_avatar(style):
    # Two crossed bars with a round face in the middle.
    body = (f'<g class="pc pc-x">{cross(50, 54, 24, 19, "{C}")}'
            f'<circle cx="50" cy="54" r="19" fill="{{C}}"/>')
    face = eyes(style, (43, 53), (57, 53))
    return f'{HEAD}{body}{face}</g></svg>'


AVATARS = []
for s in STYLES:  # alternate O and X so neighbours differ
    AVATARS.append(o_avatar(s))
    AVATARS.append(x_avatar(s))


def main():
    index = (REPO / "crates/oxbot/ui/index.html").read_text()
    glyphs = bot_glyphs(index)
    files = {
        "icon.svg": icon(),
        "icon-android-foreground.svg": android_fg(),
        "tray.svg": tray(),
        "wordmark.svg": wordmark(glyphs).replace("currentColor", "#111111"),
        "wordmark-dark.svg": wordmark(glyphs).replace("currentColor", "#FFFFFF"),
    }
    colors = ["#7A5AF8", "#E0457B", "#0F9488", "#2F6FEB", "#A0663A", "#5E6B7D",
              "#B98A00", "#2E9A4E", "#E2701B", "#0E93B8", "#B33BC9", "#7C8A1C"]
    for i, a in enumerate(AVATARS):
        files[f"avatar-{i+1:02d}.svg"] = a.replace("{C}", colors[i])
    for name, text in files.items():
        (OUT / name).write_text(text + "\n")
    (OUT / "strings.json").write_text(json.dumps({
        "avatars": AVATARS,
        "wordmark_ui": wordmark(glyphs, ui=True),
        "wordmark_plain": wordmark(glyphs),
        "icon": icon(),
    }, indent=1))
    print("wrote", len(files), "files to", OUT)


main()
