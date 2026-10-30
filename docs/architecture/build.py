#!/usr/bin/env python3
"""Builds docs/architecture/index.html from model.json and `cargo metadata`.

Run from anywhere: python3 docs/architecture/build.py
Update model.json at the end of every milestone, then rebuild and republish.
"""
import html
import json
import pathlib
import re
import subprocess

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[1]
e = html.escape

model = json.loads((HERE / "model.json").read_text())
meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--no-deps"],
    cwd=ROOT, check=True, capture_output=True, text=True).stdout)

# ---------------------------------------------------------------- crates, from the code
crates = []
for p in sorted(meta["packages"], key=lambda p: p["name"]):
    src = pathlib.Path(p["manifest_path"]).parent / "src"
    files = sorted(src.rglob("*.rs"))
    text = "".join(f.read_text() for f in files)
    loc = sum(1 for line in text.splitlines() if line.strip() and not line.strip().startswith("//"))
    test_files = files + sorted((src.parent / "tests").glob("*.rs"))
    tests = sum(len(re.findall(r"#\[(?:tokio::)?test", f.read_text())) for f in test_files)
    deps = [d["name"] for d in p["dependencies"] if d["kind"] is None]
    internal = [d for d in deps if d.startswith("oxbot")]
    external = [d for d in deps if not d.startswith("oxbot")]
    kind = "bin" if any("bin" in t["kind"] for t in p["targets"]) else "lib"
    crates.append(dict(name=p["name"], desc=p.get("description") or "", kind=kind, loc=loc,
                       tests=tests, internal=internal, external=external,
                       files=[str(f.relative_to(ROOT)) for f in files]))
by_id = {c["id"]: c for c in model["components"]}

# ---------------------------------------------------------------- diagram
W, H = 170, 64
def center(c):
    return c["x"] + W / 2, c["y"] + H / 2

def clip(c, tx, ty):
    """Point where the line from c's center toward (tx, ty) leaves c's box."""
    cx, cy = center(c)
    dx, dy = tx - cx, ty - cy
    if dx == 0 and dy == 0:
        return cx, cy
    sx = (W / 2) / abs(dx) if dx else float("inf")
    sy = (H / 2) / abs(dy) if dy else float("inf")
    s = min(sx, sy)
    return cx + dx * s, cy + dy * s

STYLE = {
    "built": 'stroke="var(--rust)" stroke-width="2" fill="var(--surface)"',
    "stub": 'stroke="var(--rust)" stroke-width="1.5" stroke-dasharray="6 3" fill="var(--surface)"',
    "planned": 'stroke="var(--muted)" stroke-width="1" stroke-dasharray="2 3" fill="var(--bg)"',
}
svg = ['<defs><marker id="ar" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
       '<path d="M0,0 L10,5 L0,10 z" fill="currentColor"/></marker></defs>']
svg.append('<rect x="5" y="110" width="905" height="450" rx="10" fill="none" stroke="var(--ok)" stroke-dasharray="6 4"/>'
           '<text x="18" y="132" fill="var(--ok)" font-weight="600">your laptop + home Wi-Fi</text>')
for ed in model["edges"]:
    a, b = by_id[ed["from"]], by_id[ed["to"]]
    (ax, ay), (bx, by) = center(a), center(b)
    x1, y1 = clip(a, bx, by)
    x2, y2 = clip(b, ax, ay)
    planned = ed["status"] == "planned"
    stroke = 'stroke="var(--muted)" stroke-dasharray="3 3"' if planned else 'stroke="currentColor"'
    svg.append(f'<line x1="{x1:.0f}" y1="{y1:.0f}" x2="{x2:.0f}" y2="{y2:.0f}" {stroke} marker-end="url(#ar)"/>')
    mx, my = (x1 + x2) / 2, (y1 + y2) / 2
    svg.append(f'<text x="{mx:.0f}" y="{my - 5:.0f}" text-anchor="middle" font-size="10.5" '
               f'fill="{"var(--muted)" if planned else "currentColor"}" class="elabel">{e(ed["label"])}</text>')
for c in model["components"]:
    x, y = c["x"], c["y"]
    op = ' opacity=".75"' if c["status"] == "planned" else ""
    svg.append(f'<g{op}><rect x="{x}" y="{y}" width="{W}" height="{H}" rx="7" {STYLE[c["status"]]}/>'
               f'<text x="{x + W / 2}" y="{y + 27}" text-anchor="middle" font-weight="600">{e(c["label"])}</text>'
               f'<text x="{x + W / 2}" y="{y + 46}" text-anchor="middle" fill="var(--muted)" font-size="11">{e(c["sub"])}</text></g>')
diagram = ('<svg viewBox="0 0 915 660" role="img" aria-label="oxbot architecture at milestone '
           f'{model["milestone"]}: built, stub and planned components and how they connect." '
           'font-family="JetBrains Mono, ui-monospace, monospace" font-size="12" fill="currentColor">'
           + "".join(svg) + "</svg>")

# ---------------------------------------------------------------- C4 diagrams (model.json "c4")
C4_KIND = {"person": "Person", "system": "Software System", "container": "Container",
           "component": "Component", "db": "Database", "folder": "Files",
           "external": ""}

def wrap(text, width):
    """Greedy word wrap to about `width` characters per line."""
    lines, cur = [], ""
    for word in text.split():
        if cur and len(cur) + 1 + len(word) > width:
            lines.append(cur)
            cur = word
        else:
            cur = f"{cur} {word}" if cur else word
    return lines + [cur] if cur else lines

def c4_box(n):
    x, y, w, h = n["x"], n["y"], n["w"], n["h"]
    t, planned = n["type"], n.get("status") == "planned"
    cx = x + w / 2
    if planned:
        style = 'fill="var(--bg)" stroke="var(--muted)" stroke-width="1.5" stroke-dasharray="6 4"'
    elif t == "system":
        style = 'fill="var(--rust-soft)" stroke="var(--rust)" stroke-width="2.5"'
    elif t == "external":
        style = 'fill="var(--code-bg)" stroke="var(--muted)" stroke-width="1"'
    elif t == "person":
        style = 'fill="var(--ok-soft)" stroke="var(--ok)" stroke-width="1.5"'
    else:
        style = 'fill="var(--surface)" stroke="var(--rust)" stroke-width="1.5"'
    out, top = [], y
    if t == "person":
        out.append(f'<circle cx="{cx:.0f}" cy="{y + 13}" r="13" {style}/>')
        top = y + 22
        out.append(f'<rect x="{x}" y="{top}" width="{w}" height="{h - 22}" rx="18" {style}/>')
    elif t == "db":
        out.append(f'<path d="M{x},{y + 9} a{w / 2},9 0 0 1 {w},0 v{h - 18} a{w / 2},9 0 0 1 {-w},0 z" {style}/>'
                   f'<path d="M{x},{y + 9} a{w / 2},9 0 0 0 {w},0" fill="none" {style.split(" ", 1)[1]}/>')
        top = y + 12
    elif t == "folder":
        out.append(f'<path d="M{x},{y + 10} h{w * 0.35:.0f} l8,-10 h{w * 0.65 - 8:.0f} v{h} h{-w} z" {style}/>')
        top = y + 6
    else:
        if n.get("stack"):  # several of the same thing: copies peek out behind
            for off in (10, 5):
                out.append(f'<rect x="{x + off}" y="{y - off}" width="{w}" height="{h}" rx="{8 if t == "component" else 6}" {style}/>')
        out.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{8 if t == "component" else 6}" {style}/>')
    kind = C4_KIND[t] if t != "external" else "External"
    tag = f"[{kind}: {n['tech']}]" if n.get("tech") and t not in ("person", "system") else f"[{kind}]"
    if t == "external":
        tag = f"[{n['tech']}]" if n.get("tech") else "[External]"
    muted = planned or t == "external"
    ink = "var(--muted)" if muted else "currentColor"
    out.append(f'<text x="{cx:.0f}" y="{top + 21}" text-anchor="middle" font-size="13" font-weight="600" fill="{ink}">{e(n["name"])}</text>')
    tag_lines = wrap(tag, int((w - 14) / 6.7))
    for i, line in enumerate(tag_lines):
        out.append(f'<text x="{cx:.0f}" y="{top + 37 + i * 13}" text-anchor="middle" font-size="11" fill="var(--muted)" class="mono">{e(line)}</text>')
    dy = 56 + 13 * (len(tag_lines) - 1)
    for i, line in enumerate(wrap(n.get("desc", ""), int((w - 18) / 5.9))):
        out.append(f'<text x="{cx:.0f}" y="{top + dy + i * 14}" text-anchor="middle" font-size="11" fill="{ink}">{e(line)}</text>')
    if planned:  # a "planned" tab sitting on the top edge
        out.append(f'<rect x="{x + w - 70}" y="{y - 8}" width="60" height="16" rx="8" fill="var(--surface)" stroke="var(--muted)"/>'
                   f'<text x="{x + w - 40}" y="{y + 4}" text-anchor="middle" font-size="11" fill="var(--muted)" class="mono">planned</text>')
    return "".join(out)

def c4_clip(n, tx, ty):
    """Where the line from n's center toward (tx, ty) leaves n's box."""
    cx, cy = n["x"] + n["w"] / 2, n["y"] + n["h"] / 2
    dx, dy = tx - cx, ty - cy
    if dx == 0 and dy == 0:
        return cx, cy
    s = min((n["w"] / 2) / abs(dx) if dx else float("inf"), (n["h"] / 2) / abs(dy) if dy else float("inf"))
    return cx + dx * s, cy + dy * s

def c4_svg(lv):
    g, nodes = lv["grid"], {}
    for n in lv["nodes"]:
        n = dict(n)
        span = n.get("span", 1)
        n.setdefault("x", g["x0"] + n["col"] * g["step"])
        n.setdefault("w", g["w"] + (span - 1) * g["step"])
        n.setdefault("y", g["rows"][n["row"]])
        nodes[n["id"]] = n
    mid = f'c4-{lv["id"]}'
    out = [f'<defs>'
           f'<marker id="{mid}-a" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="var(--ink)"/></marker>'
           f'<marker id="{mid}-p" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="var(--muted)"/></marker>'
           f'</defs>']
    for b in lv.get("boundaries", []):
        out.append(f'<rect x="{b["x"]}" y="{b["y"]}" width="{b["w"]}" height="{b["h"]}" rx="10" fill="none" stroke="var(--ok)" stroke-width="1.5" stroke-dasharray="8 5"/>'
                   f'<text x="{b["x"] + 12}" y="{b["y"] + 20}" fill="var(--ok)" font-size="12" font-weight="600">{e(b["label"])}</text>')
    labels = []
    for ed in lv["edges"]:
        a, b = nodes[ed["from"]], nodes[ed["to"]]
        via = [tuple(p) for p in ed.get("via", [])]
        bc = (b["x"] + b["w"] / 2, b["y"] + b["h"] / 2)
        ac = (a["x"] + a["w"] / 2, a["y"] + a["h"] / 2)
        start = tuple(ed["from_pt"]) if "from_pt" in ed else c4_clip(a, *(via[0] if via else bc))
        end = tuple(ed["to_pt"]) if "to_pt" in ed else c4_clip(b, *(via[-1] if via else (start if "from_pt" in ed else ac)))
        pts = [start, *via, end]
        planned = ed.get("status") == "planned"
        m = f'{mid}-{"p" if planned else "a"}'
        stroke = 'stroke="var(--muted)" stroke-dasharray="5 4"' if planned else 'stroke="var(--ink)"'
        both = f' marker-start="url(#{m})"' if ed.get("both") else ""
        out.append(f'<polyline points="{" ".join(f"{x:.0f},{y:.0f}" for x, y in pts)}" fill="none" {stroke} stroke-width="1.3"'
                   f' marker-end="url(#{m})"{both}/>')
        if "label_at" in ed:
            lx, ly, anchor = ed["label_at"]
        else:  # middle of the longest segment, just above it
            (x1, y1), (x2, y2) = max(zip(pts, pts[1:]), key=lambda s: (s[1][0] - s[0][0]) ** 2 + (s[1][1] - s[0][1]) ** 2)
            n_lines = ed["label"].count("\n") + 1
            lx, ly, anchor = (x1 + x2) / 2, (y1 + y2) / 2 - 6 - 13 * (n_lines - 1), "middle"
        fill = "var(--muted)" if planned else "currentColor"
        spans = "".join(f'<tspan x="{lx:.0f}" dy="{0 if i == 0 else 13}">{e(s)}</tspan>'
                        for i, s in enumerate(ed["label"].split("\n")))
        labels.append(f'<text x="{lx:.0f}" y="{ly:.0f}" text-anchor="{anchor}" font-size="11" fill="{fill}" class="elabel">{spans}</text>')
    out += [c4_box(n) for n in nodes.values()] + labels
    w, h = lv["size"]
    return (f'<svg viewBox="0 0 {w} {h}" role="img" aria-label="{e(lv["aria"])}" '
            f'font-size="12" fill="currentColor">' + "".join(out) + "</svg>")

c4 = "".join(
    f'<figure class="c4fig" id="c4-{lv["id"]}"><h3>{e(lv["title"])}</h3><p class="small">{e(lv["intro"])}</p>'
    f'<div class="diagram c4d-{lv["id"]}">{c4_svg(lv)}</div>'
    f'<figcaption>{e(lv["caption"])}</figcaption></figure>'
    for lv in model["c4"]["levels"])

# ---------------------------------------------------------------- tables
status_of = {c["id"]: c for c in model["components"]}
mlabel = lambda m: f"M{m}" if str(m)[:1].isdigit() else str(m)
pill = lambda s: f'<span class="pill {s}">{s}</span>'
crate_rows = "".join(
    f'<tr><td><code>{e(c["name"])}</code><div class="small">{c["kind"]}</div></td>'
    f'<td>{e(c["desc"])}</td>'
    f'<td>{pill(status_of.get(c["name"], {}).get("status", "built"))}</td>'
    f'<td class="num">{c["loc"]}</td><td class="num">{c["tests"]}</td>'
    f'<td class="small">{", ".join(e(d) for d in c["internal"]) or "–"}</td>'
    f'<td class="small">{", ".join(e(d) for d in c["external"]) or "–"}</td></tr>'
    for c in crates)
other_rows = "".join(
    f'<tr><td>{e(c["label"])}</td><td>{e(c["sub"])}</td><td>{pill(c["status"])}</td><td class="num">{mlabel(c["m"])}</td></tr>'
    for c in model["components"] if c["kind"] != "crate")
history = "".join(
    f'<li><span class="n">{e(mlabel(h["m"]))}</span><div><h3>{e(h["title"])} <span class="small">{h["date"]}</span></h3>'
    f'<ul>{"".join(f"<li>{e(i)}</li>" for i in h["items"])}</ul></div></li>'
    for h in reversed(model["history"]))
nx = model["next"]
ch = model.get("changed", {})
changed = (f'<p class="small">{e(ch.get("intro", ""))}</p><ul class="changed">'
           + "".join(f'<li><b>{e(i["title"])}.</b> {e(i["text"])}</li>' for i in ch.get("items", [])) + "</ul>")
tot_loc = sum(c["loc"] for c in crates)
tot_tests = sum(c["tests"] for c in crates)

page = (HERE / "template.html").read_text()
for k, v in {
    "{{MILESTONE}}": str(model["milestone"]), "{{UPDATED}}": model["updated"],
    "{{DIAGRAM}}": diagram, "{{C4}}": c4,"{{CRATES}}": crate_rows, "{{OTHERS}}": other_rows,
    "{{HISTORY}}": history, "{{CHANGED}}": changed, "{{NEXT}}": f'<b>{e(mlabel(nx["m"]))} · {e(nx["title"])}</b> {e(nx["text"])}',
    "{{NCRATES}}": str(len(crates)), "{{LOC}}": str(tot_loc), "{{TESTS}}": str(tot_tests),
}.items():
    page = page.replace(k, v)
(HERE / "index.html").write_text(page)
print(f"wrote {HERE / 'index.html'}: {len(crates)} crates, {tot_loc} lines, {tot_tests} tests")
