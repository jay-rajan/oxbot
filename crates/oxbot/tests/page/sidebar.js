// Run by tests/page.rs: how the sidebar groups, files and finds bots.
const fs = require("fs");
const src = fs.readFileSync(process.argv[2], "utf8");
const { load } = require("./fake-dom.js");
const t = load(src, `{ groupBots, onMessage, renderBots, fold,
  state: () => ({ sections, placed }), setWs: (w) => { ws = w; } }`);
const sent = [];
t.setWs({ readyState: 1, send: (m) => sent.push(JSON.parse(m)) });
let ok = true;
const check = (cond, what) => { if (!cond) { ok = false; console.log("FAIL", what); } else console.log("ok  ", what); };
const shape = (groups) => groups.map((g) => `${g.title || g.key}:${g.ids.join("+")}`).join(" | ");

const list = [
  { id: "recipes", name: "Recipes", created_by: "Jay" },
  { id: "assistant", name: "Herder", central: true },
  { id: "bank", name: "Bank", created_by: "Sam", private: true, locked: true },
  { id: "cafe", name: "Café Hours", created_by: "Ann" },
  { id: "journal", name: "Journal", created_by: "Jay", private: true },
  { id: "old", name: "Old" },
];
const secs = [{ id: "s_1", name: "Kitchen" }, { id: "s_2", name: "Money" }];
const placed = { recipes: "s_1", cafe: "s_1", bank: "s_2", journal: "s_gone" };

// By section: Herder, bots not in one, then each section in order.
let g = t.groupBots(list, secs, placed, "sections", "", "Jay");
check(shape(g) === "herder:assistant | none:journal+old | Kitchen:recipes+cafe | Money:bank",
  "Herder first, then bots not in a section, then sections in order: " + shape(g));
check(g[1].ids.includes("journal"), "a placement in a section that's gone is ignored");
// An empty section still shows (you can drop into it); without bots outside sections there's no untitled group.
g = t.groupBots(list, [...secs, { id: "s_3", name: "Empty" }], { ...placed, journal: "s_1", old: "s_1" }, "sections", "", "Jay");
check(shape(g) === "herder:assistant | Kitchen:recipes+cafe+journal+old | Money:bank | Empty:", "empty sections show: " + shape(g));

// By person: Herder, you first, the rest A-Z, the home computer's (no maker) last.
g = t.groupBots(list, secs, placed, "people", "", "Jay");
check(shape(g) === "herder:assistant | Jay:recipes+journal | Ann:cafe | Sam:bank | Home computer:old", "you first, then A-Z, then the home computer: " + shape(g));
check(g[0].title === "" && g[0].ids[0] === "assistant", "Herder pinned outside any person");
g = t.groupBots(list, secs, placed, "people", "", "Sam");
check(g[1].title === "Sam", "whoever you are comes first");

// Search: case- and accent-insensitive, Herder too, empty groups hidden.
g = t.groupBots(list, secs, placed, "sections", "cafe", "Jay");
check(shape(g) === "Kitchen:cafe", "\"cafe\" finds Café, and empty groups go: " + shape(g));
g = t.groupBots(list, secs, placed, "sections", "  RECI ", "Jay");
check(shape(g) === "Kitchen:recipes", "case and spaces don't matter");
g = t.groupBots(list, secs, placed, "people", "herd", "Jay");
check(shape(g) === "herder:assistant", "Herder can be found");
g = t.groupBots(list, secs, placed, "sections", "nothing like it", "Jay");
check(g.length === 0, "no match: no groups");
check(t.fold("Ünïcödé") === "unicode", "fold drops accents");

// The server's sections message fills the state (and survives odd input).
t.onMessage({ type: "bots", bots: list });
t.onMessage({ type: "sections", sections: secs, placed: { recipes: "s_1" } });
let st = t.state();
check(st.sections.length === 2 && st.placed.recipes === "s_1", "sections message is kept");
check(st.placed.constructor === undefined, "placements are a plain lookup (no inherited keys)");
t.onMessage({ type: "sections" });
st = t.state();
check(st.sections.length === 0 && Object.keys(st.placed).length === 0, "a message without lists clears them");
t.renderBots();
console.log(ok ? "ALL OK" : "SOME FAILED");
process.exit(ok ? 0 : 1);
