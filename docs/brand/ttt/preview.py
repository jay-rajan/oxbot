"""Build docs/brand/ttt/preview.html (live preview of the new look). Usage: python3 -I preview.py <repo>"""
import json
import re
import sys
from pathlib import Path

R = Path(sys.argv[1])
S = json.loads((R / "docs/brand/ttt/strings.json").read_text())
ui = (R / "crates/oxbot/ui/index.html").read_text()
js = ui[ui.index("// ---------- the board animation"):ui.index('// Live "Working…" line')]
herder = json.loads(re.search(r'^const HERDER = (".*?"); //', ui, re.M).group(1))
avs = S["avatars"]
colors = ["#7A5AF8", "#E0457B", "#0F9488", "#2F6FEB", "#A0663A", "#5E6B7D",
          "#B98A00", "#2E9A4E", "#E2701B", "#0E93B8", "#B33BC9", "#7C8A1C"]
icon, wm = S["icon"], S["wordmark_plain"]
tray = (R / "docs/brand/ttt/tray.svg").read_text().strip()
fg = (R / "docs/brand/ttt/icon-android-foreground.svg").read_text().strip()


def avatar(i, busy=False):
    return f'<span class="avatar{" working" if busy else ""}">{avs[i].replace("{C}", colors[i])}</span>'


BOTS = [("Groceries", "shared"), ("Meal plan", "working…"), ("Bank", "just me"),
        ("Chores", "shared"), ("Home to-do", "working…"), ("Divit learning", "just me")]
rows = "".join(f'<div class="botrow">{avatar(i, s == "working…")}<div><b>{n}</b><small>{s}</small></div></div>'
               for i, (n, s) in enumerate(BOTS))
allav = "".join(avatar(i) for i in range(12))
sizes = "".join(f'<div class="sz">{icon.replace("<svg ", f"<svg width=\"{s}\" height=\"{s}\" ", 1)}{s}</div>'
                for s in (180, 96, 60, 32, 16))
blank = '<span class="ic"></span>'
phone = (blank * 5 + f'<span class="lbl"><span class="ox">{fg}</span>oxbot</span>' + blank * 6)


def app(theme):
    return f'''<div class="app {theme}">
  <div class="side"><div class="wm">{wm}</div>
   <div class="botrow sel"><span class="avatar working">{herder}</span><div><b>Assistant</b><small>leads your bots</small></div></div>{rows}</div>
  <div class="main"><div class="msg me">Add eggs and bananas to the grocery list</div>
   <div class="typing"><span class="ttt"></span><span>Reading the grocery list</span></div>
   <div class="chip"><span class="spin"></span>receipts.csv · uploading</div></div></div>'''


CSS = """
:root{--bg:#0a0a0b;--fg:#f2f0eb;--muted:#9a9aa2;--edge:#2a2a2e;color-scheme:dark}
body{background:var(--bg);color:var(--fg);font:15px/1.55 -apple-system,"Segoe UI",Inter,sans-serif}
.wrap{max-width:1040px;margin:0 auto;padding-inline:16px;padding-block:36px 80px;display:flex;flex-direction:column;gap:40px}
h1{font-size:30px;margin:0;line-height:1.2;text-wrap:balance} h2{font-size:19px;margin:0 0 12px} p{margin:0;max-width:70ch} .muted{color:var(--muted)}
.eyebrow{font-size:12px;letter-spacing:.12em;text-transform:uppercase;color:var(--muted);margin-bottom:6px}
.row{display:flex;flex-wrap:wrap;align-items:flex-end;gap:22px}
.sz{display:flex;flex-direction:column;align-items:center;gap:6px;font-size:12px;color:var(--muted)} .sz svg{display:block}
.menubar{background:#e9e9ec;color:#000;border-radius:8px;height:30px;display:flex;align-items:center;justify-content:flex-end;gap:16px;padding-inline:14px;font-size:13px;max-width:420px}
.menubar.dk{background:#2b2b2f;color:#fff} .menubar.dk svg *{stroke:#fff} .menubar svg{width:18px;height:18px}
.phone{width:240px;max-width:100%;border-radius:26px;padding:18px;background:linear-gradient(160deg,#3b4d7a,#1d2238);display:grid;grid-template-columns:repeat(4,1fr);gap:14px;justify-items:center;font-size:10px;color:#fff}
.phone .ic{width:40px;height:40px;border-radius:50%;background:#ffffff22} .phone .lbl{display:flex;flex-direction:column;align-items:center;gap:4px}
.phone .ox{width:40px;height:40px;border-radius:50%;overflow:hidden;background:#111;display:block} .phone .ox svg{width:100%;height:100%;display:block}
.app{--focus:#8A8A86;--ink:#1c1d20;--surface:#fff;--line:#e3e1dc;--rust:#FF3737;--soft:#f5f4f1;--mut:#6b6e75;display:grid;grid-template-columns:minmax(0,230px) minmax(0,1fr);border-radius:12px;overflow:hidden;border:1px solid var(--edge);background:var(--surface);color:var(--ink);font-size:13px}
.app.dark{--focus:#8c8c92;--ink:#ecebe7;--surface:#1b1c1f;--line:#3a3b40;--rust:#FF4A4A;--soft:#26272b;--mut:#a3a6ad}
.side{border-right:1px solid var(--line);padding:14px;display:flex;flex-direction:column;gap:4px} .wm svg{height:30px;width:auto;color:var(--ink);margin-bottom:8px;display:block}
.botrow{display:flex;align-items:center;gap:10px;padding:6px;border-radius:8px} .botrow.sel{background:var(--soft)} .botrow b{display:block;font-size:13px} .botrow small{color:var(--mut);font-size:11px}
.main{padding:16px;display:flex;flex-direction:column;gap:12px;min-width:0}
.msg.me{align-self:flex-end;background:var(--soft);padding:8px 12px;border-radius:12px;max-width:80%}
.avatar{width:34px;height:34px;flex:none;display:grid;place-items:center} .avatar svg{width:100%;height:100%;display:block} svg.herder{color:var(--ink)}
.avatar .pc{transform-box:view-box;transform-origin:50px 54px}
.avatar.working .pc-o{animation:ttt-rock 1.4s ease-in-out infinite} .avatar.working .pc-x{animation:ttt-tick 1.4s ease-in-out infinite}
@keyframes ttt-rock{0%,100%{transform:rotate(0)}25%{transform:rotate(-16deg)}75%{transform:rotate(16deg)}}
@keyframes ttt-tick{0%,40%,80%,100%{transform:scale(1) rotate(0)}20%{transform:scale(1.08) rotate(-10deg)}60%{transform:scale(1.08) rotate(10deg)}}
.typing{display:flex;align-items:center;gap:6px;color:var(--rust);font-size:12px} .typing .ttt{width:18px;height:18px} .typing svg{width:100%;height:100%;display:block}
.chip{align-self:flex-start;display:flex;align-items:center;gap:8px;border:1px solid var(--line);border-radius:999px;padding:4px 10px;font-size:12px}
.spin{width:12px;height:12px;position:relative;flex:none}
.spin::before{content:"";position:absolute;inset:1px;border:2px solid var(--ink);border-radius:50%;animation:ttt-turn 1s linear infinite}
.spin::after{content:"";position:absolute;inset:0;background:linear-gradient(45deg,transparent 40%,var(--rust) 40% 60%,transparent 60%),linear-gradient(-45deg,transparent 40%,var(--rust) 40% 60%,transparent 60%);animation:ttt-turn 1s linear -.5s infinite}
@keyframes ttt-turn{0%,49.9%{opacity:1}50%,100%{opacity:0}}
.welcome{display:flex;flex-wrap:wrap;gap:20px;align-items:center}
.card{--ink:#1c1d20;--line:#e3e1dc;--rust:#FF3737;background:#fff;color:#1c1d20;border-radius:14px;padding:20px;width:300px;max-width:100%;display:flex;flex-direction:column;gap:10px}
.card.dark{--ink:#ecebe7;--line:#3a3b40;--rust:#FF4A4A;background:#1b1c1f;color:#ecebe7}
.hero{width:72px;height:72px} .hero svg{width:100%;height:100%;display:block}
button{font:inherit;background:#FF3737;color:#000;border:0;border-radius:8px;padding:6px 14px;font-weight:600;cursor:pointer}
.avgrid{display:flex;flex-wrap:wrap;gap:14px;background:#fff;padding:14px;border-radius:10px} .avgrid.dk{background:#1b1c1f} .avgrid .avatar{width:48px;height:48px}
@media (max-width:640px){.app{grid-template-columns:1fr}.side{border-right:0;border-bottom:1px solid var(--line)}}
@media (prefers-reduced-motion: reduce){.avatar.working .pc,.spin::before,.spin::after{animation:none}}
"""

html = f'''<title>oxbot New Look</title>
<style>{CSS}</style>
<div class="wrap">
<header><div class="eyebrow">oxbot · rebrand preview · branch rebrand</div><h1>O and X: the new look, live</h1>
<p class="muted">What's built: the app icon, the menu-bar icon, the Android icon, the wordmark, 12 bot pieces (6 O, 6 X), the Assistant as the board, and four animations. The ox heads and horns are gone. App words don't change.</p></header>

<section><div class="eyebrow">1 · Icons</div><h2>App icon, menu bar, phone</h2>
<div class="row">{sizes}</div>
<div class="row" style="margin-top:20px;align-items:center">
<div style="display:flex;flex-direction:column;gap:8px;flex:1;min-width:240px"><div class="menubar">{tray}<span>Wi-Fi</span><span>Fri 11:20</span></div><div class="menubar dk">{tray}<span>Wi-Fi</span><span>Fri 11:20</span></div><span class="muted" style="font-size:12px">Menu bar: a template icon, so macOS colors it for light and dark.</span></div>
<div class="phone">{phone}</div>
</div></section>

<section><div class="eyebrow">2 · In the app</div><h2>Sidebar, working bots, the Working… line, the upload chip</h2>
<p class="muted" style="margin-bottom:14px">A working O rocks like a rolling ring; a working X ticks (tic… tac). The Working… line shows marks taking turns. Uploading: an O and an X swap. With reduced motion everything stays still.</p>
<div style="display:flex;flex-direction:column;gap:16px">{app("light")}{app("dark")}</div></section>

<section><div class="eyebrow">3 · Welcome</div><h2>First start: the board plays one game</h2>
<div class="welcome"><div class="card"><div class="hero" data-hero></div><b>Welcome to oxbot</b><span style="font-size:13px">Tell oxbot who you are…</span></div>
<div class="card dark"><div class="hero" data-hero></div><b>Welcome to oxbot</b><span style="font-size:13px">Tell oxbot who you are…</span></div>
<button type="button" id="replay">Play again</button></div></section>

<section><div class="eyebrow">4 · All 12 bot pieces</div><h2>Each bot gets a piece and a colour from its name</h2>
<div style="display:flex;flex-direction:column;gap:10px"><div class="avgrid">{allav}</div><div class="avgrid dk">{allav}</div></div></section>
</div>
<script>
{js}
const hero = () => document.querySelectorAll("[data-hero]").forEach((e) => (e.innerHTML = tttBoard({{ once: true }})));
hero(); document.getElementById("replay").onclick = hero;
document.querySelectorAll(".typing .ttt").forEach((e) => (e.innerHTML = tttBoard()));
</script>
'''
(R / "docs/brand/ttt/preview.html").write_text(html)
print("preview", len(html))
