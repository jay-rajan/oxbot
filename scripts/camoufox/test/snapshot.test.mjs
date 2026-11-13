import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { toDevtools } from "../snapshot.mjs";

// Captured from Camoufox on a local shop page (Playwright 1.62, mode "ai").
const aria = await readFile(new URL("./fixtures/shop.aria.txt", import.meta.url), "utf8");
const convert = (text) => {
  let n = 0;
  return toDevtools(text, {
    title: "Corner Grocer",
    url: "http://127.0.0.1:8765/",
    uid: (i) => (i === "root" ? "s_root" : `s_${n++}`),
  });
};
const line = (lines, needle) => {
  const found = lines.find((l) => l.includes(needle));
  assert.ok(found, `no line with ${needle}:\n${lines.join("\n")}`);
  return found;
};

test("nodes keep their depth, two spaces per level under the root", () => {
  const { lines } = convert(aria);
  assert.equal(lines[0], 'uid=s_root RootWebArea "Corner Grocer" url="http://127.0.0.1:8765/"');
  assert.match(line(lines, "uid=s_0 generic"), /^ {2}uid=/);
  assert.match(line(lines, 'link "Home"'), /^ {8}uid=/);
  assert.match(line(lines, 'heading "Fresh produce"'), /^ {6}uid=\S+ heading "Fresh produce" level="1"$/);
});

test("links carry absolute addresses and no Playwright-only flags", () => {
  const { lines } = convert(aria);
  assert.match(line(lines, 'link "Home"'), / url="http:\/\/127\.0\.0\.1:8765\/"$/);
  assert.match(line(lines, 'link "Organic bananas"'), / url="http:\/\/127\.0\.0\.1:8765\/p\/1"$/);
  assert.match(line(lines, 'link "Partner: deals"'), / url="https:\/\/other\.example\/x\?y=1"$/);
  assert.ok(!lines.some((l) => /\/url:|cursor|\bref=/.test(l)), lines.join("\n"));
});

test("text is StaticText, a field's text is its value, and states are flags", () => {
  const { lines } = convert(aria);
  assert.match(line(lines, '"$0.59 / lb"'), /StaticText "\$0\.59 \/ lb"$/);
  assert.match(line(lines, '"Prices include tax."'), /StaticText/);
  assert.match(line(lines, 'StaticText "Note:'), /StaticText "Note: \\"quoted\\" text: here"$/);
  assert.match(line(lines, 'textbox "Email"'), / focused value="a@b\.c"$/);
  assert.match(line(lines, 'checkbox "Keep me"'), / checked$/);
  assert.match(line(lines, 'button "Pay"'), / disabled$/);
  assert.match(line(lines, 'option "1"'), / selected$/);
  assert.ok(!lines.some((l) => l.includes('StaticText ""')), "no empty text lines");
});

test("every node with a Playwright ref can be acted on by its uid", () => {
  const { lines, refs } = convert(aria);
  const milk = refs.find((r) => r.name === "Whole milk, 1 gal");
  assert.deepEqual(milk, { uid: milk.uid, ref: "e18", role: "link", name: "Whole milk, 1 gal" });
  assert.ok(line(lines, `uid=${milk.uid} link`));
  assert.ok(!refs.some((r) => r.role === "option"), "options have no ref of their own");
});
