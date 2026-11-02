// Run by tests/page.rs: the order of open cards, and when they redraw.
const fs = require("fs");
const src = fs.readFileSync(process.argv[2], "utf8");
const { load } = require("./fake-dom.js");
const t = load(src, `{ onMessage, cardList, renderCards,
  countCards: (f) => { const d = div; div = (c) => { if (String(c).startsWith("card")) f(c); return d(c); }; } }`);
let ok = true;
const check = (cond, what) => { if (!cond) { ok = false; console.log("FAIL", what); } else console.log("ok  ", what); };
const order = () => t.cardList().map((c) => c.key).join(",");
let drawn = 0;
t.countCards(() => { drawn++; });

t.onMessage({ type: "bots", bots: [{ id: "recipes", name: "Recipes" }, { id: "journal", name: "Journal" }] });
t.onMessage({ type: "approval_needed", id: "1", bot: "recipes", kind: "change", action: "click", target: "Post", site: "x" });
t.onMessage({ type: "sign_in_needed", id: "1", bot: "journal", site: "y" });
t.onMessage({ type: "approval_needed", id: "2", bot: "journal", kind: "change", action: "click", target: "Post", site: "x" });
check(order() === "a:1,s:1,a:2", "oldest first, newest last: a sign-in after an approval comes after it");

t.onMessage({ type: "approval_needed", id: "1", bot: "recipes", kind: "change", action: "click", target: "Post", site: "x" });
check(order() === "a:1,s:1,a:2", "a card sent again keeps its place");
t.onMessage({ type: "sign_in_needed", id: "1", bot: "journal", site: "y", window_open: true });
check(order() === "a:1,s:1,a:2", "a sign-in that changes keeps its place too");

t.onMessage({ type: "approval_resolved", id: "1" });
t.onMessage({ type: "approval_needed", id: "3", bot: "recipes", kind: "change", action: "click", target: "Post", site: "x" });
check(order() === "s:1,a:2,a:3", "an answered card goes; the next new one is last");

drawn = 0;
t.onMessage({ type: "bots", bots: [{ id: "recipes", name: "Recipes" }, { id: "journal", name: "Journal" }] });
check(drawn === 0, "a bots update with the same names doesn't redraw the cards");
t.onMessage({ type: "bots", bots: [{ id: "recipes", name: "Family Recipes" }, { id: "journal", name: "Journal" }] });
check(drawn === 3, "a renamed bot redraws them");

t.onMessage({ type: "hello", server_name: "Mac" });
check(order() === "", "hello clears the cards (the server sends them again)");
t.onMessage({ type: "approval_needed", id: "2", bot: "journal", kind: "change", action: "click", target: "Post", site: "x" });
t.onMessage({ type: "approval_needed", id: "3", bot: "recipes", kind: "change", action: "click", target: "Post", site: "x" });
check(order() === "a:2,a:3", "sent again after hello, in the server's order");
console.log(ok ? "ALL OK" : "SOME FAILED");
process.exit(ok ? 0 : 1);
