// Run by tests/page.rs: the page script (path in argv[2]) against a fake DOM.
const fs = require("fs");
const src = fs.readFileSync(process.argv[2], "utf8");
function fake() {
  const f = function () { return fake(); };
  return new Proxy(f, {
    get(t, k) {
      if (k === Symbol.toPrimitive) return () => "";
      if (k === "length" ) return 0;
      if (k === Symbol.iterator) return function* () {};
      if (k in t && typeof k === "string" && ["scrollTop","scrollHeight"].includes(k)) return t[k];
      if (k === "then") return undefined;
      if (["scrollTop", "scrollHeight", "clientHeight"].includes(k)) return 0;
      if (k === "dataset" || k === "style" || k === "classList") return fake();
      return t[k] !== undefined && typeof k === "string" && !["name","call","apply","bind"].includes(k) ? t[k] : fake();
    },
    set(t, k, v) { t[k] = v; return true; },
    apply() { return fake(); },
  });
}
const ctx = {
  document: fake(), window: { addEventListener() {} }, location: { hash: "", search: "", pathname: "/", host: "x" },
  history: { replaceState() {} }, sessionStorage: { getItem() { return ""; }, setItem() {} },
  localStorage: { getItem() { return null; }, setItem() {} }, navigator: {}, requestAnimationFrame() {},
  setTimeout() {}, clearTimeout() {}, setInterval() {}, clearInterval() {}, WebSocket: function () { return fake(); },
  console, TextEncoder, URLSearchParams, Math, JSON, Date, Map, Set, Array, Object, String, Number, Promise,
  encodeURIComponent, matchMedia: () => fake(), Intl, alert() {}, confirm() { return true; },
};
const vm = require("vm");
vm.createContext(ctx);
vm.runInContext(src + "\n;globalThis.__t = { bots, onMessage, select, setWs: (w) => { ws = w; } };", ctx);
const t = ctx.__t;
const sent = [];
t.setWs({ readyState: 1, send: (m) => sent.push(JSON.parse(m)) });
const ev = (bot, seq) => ({ bot, seq, ts_ms: 0, kind: "tool_use", name: "Read", summary: "s" + seq });
const range = (a, b) => Array.from({ length: b - a + 1 }, (_, i) => a + i);
let ok = true;
const check = (cond, what) => { if (!cond) { ok = false; console.log("FAIL", what); } else console.log("ok  ", what); };

// 1. A fresh window after a long time away replaces local events.
t.onMessage({ type: "history", bot: "a", events: range(1, 5).map((s) => ev("a", s)), first_seq: 1, has_older: false });
let b = t.bots.get("a");
check(b.events.length === 5 && b.lastSeq === 5, "first page kept");
t.onMessage({ type: "history", bot: "a", events: range(1001, 1500).map((s) => ev("a", s)), first_seq: 1001, has_older: true });
check(b.events.length === 500 && b.events[0].seq === 1001 && b.lastSeq === 1500, "fresh window replaces the old events");
check(b.firstSeq === 1001 && b.hasOlder === true, "and pages back from its start");
// A latest page that overlaps adds only what's new.
t.onMessage({ type: "history", bot: "a", events: range(1490, 1502).map((s) => ev("a", s)), first_seq: 1490, has_older: true });
check(b.events.length === 502 && b.lastSeq === 1502, "overlapping page appends the new ones");

// 2. An empty older page (nothing readable) still moves back.
b.loadingOlder = true;
t.onMessage({ type: "history", bot: "a", events: [], first_seq: 801, has_older: true });
check(b.firstSeq === 801 && b.hasOlder === true && b.events.length === 502, "empty older page moves firstSeq back");
b.loadingOlder = true;
t.onMessage({ type: "history", bot: "a", events: [], first_seq: 0, has_older: false });
check(b.hasOlder === false, "an empty page with nothing before ends paging");
// A normal older page goes in front.
b.loadingOlder = true; b.hasOlder = true;
t.onMessage({ type: "history", bot: "a", events: range(700, 800).map((s) => ev("a", s)), first_seq: 700, has_older: true });
check(b.events[0].seq === 700 && b.firstSeq === 700, "older page goes in front");

// 3. Leaving a long chat keeps its last few pages.
t.onMessage({ type: "bots", bots: [{ id: "a", name: "A" }, { id: "b", name: "B" }] });
t.select("a");
const before = b.events.length;
t.select("b");
check(before > 600 && b.events.length === 600 && b.events[b.events.length - 1].seq === 1502, "left chat trimmed to 3 pages, newest kept");
check(b.firstSeq === b.events[0].seq && b.hasOlder === true, "and can page back again");
console.log(ok ? "ALL OK" : "SOME FAILED");
process.exit(ok ? 0 : 1);
