// Run by tests/page.rs: a schedule card's Change fields turn back into the
// same small rule the server takes.
const fs = require("fs");
const src = fs.readFileSync(process.argv[2], "utf8");
const { load } = require("./fake-dom.js");
const t = load(src, `{ onMessage, cardList, editOf, ruleOf, schedEdits }`);
let ok = true;
const check = (cond, what) => { if (!cond) { ok = false; console.log("FAIL", what); } else console.log("ok  ", what); };
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const card = (id, rule) => ({ type: "approval_needed", id, bot: "prep", kind: "schedule", action: "set up a schedule",
  target: "x", site: "oxbot", schedule: { task: "Send me today's prep plan.", rule, text: "x", tz: "UTC", bot_name: "Prep" } });

for (const [id, rule] of [
  [1, { kind: "weekdays", time: "08:15" }],
  [2, { kind: "weekly", days: ["mon", "sat"], time: "18:00" }],
  [3, { kind: "monthly", day: 31, time: "09:00" }],
  [4, { kind: "once", at: "2026-10-20T09:00" }],
]) {
  const m = card(id, rule);
  t.onMessage(m);
  check(same(t.ruleOf(t.editOf(m)), rule), `${rule.kind} comes back as it was`);
}
check(t.cardList().length === 4, "schedule cards are listed with the others");
const e = t.editOf(card(1, { kind: "weekdays", time: "08:15" }));
e.kind = "weekly"; e.days.add("sun"); e.days.add("tue"); e.time = "07:30";
check(same(t.ruleOf(e), { kind: "weekly", days: ["tue", "sun"], time: "07:30" }), "days come in week order");
t.onMessage({ type: "approval_resolved", id: 1 });
check(!t.schedEdits.has(1) && t.schedEdits.has(2), "an answered card's changes go");
console.log(ok ? "ALL OK" : "SOME FAILED");
process.exit(ok ? 0 : 1);
