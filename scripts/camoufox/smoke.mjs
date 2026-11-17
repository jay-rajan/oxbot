// Integration smoke test. Only local fixtures are visited; never a retailer.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { createReadStream } from "node:fs";
import { createHash } from "node:crypto";
import {
  mkdtemp,
  mkdir,
  readFile,
  rm,
  writeFile,
  readdir,
  readlink,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

// Diagnostic guard for dependency downloads, without adding an OS sandbox
// that prevents upstream Firefox itself from starting on this machine.
const noDownloads = process.env.OXBOT_CAMOUFOX_SMOKE_NO_DOWNLOADS === "1";
if (noDownloads) {
  globalThis.fetch = async () => {
    throw new Error("Node runtime download is forbidden in this test");
  };
  const net = await import("node:net");
  const original = net.Socket.prototype.connect;
  net.Socket.prototype.connect = function (...args) {
    let first = Array.isArray(args[0]) ? args[0][0] : args[0];
    const unix = typeof first === "string" && !/^\d+$/.test(first);
    const host =
      typeof first === "object"
        ? first?.host
        : typeof args[1] === "string"
          ? args[1]
          : "localhost";
    if (!unix && host && !["localhost", "127.0.0.1", "::1"].includes(host))
      throw new Error("Node outbound network is forbidden in this test");
    return original.apply(this, args);
  };
}

const bundle = process.env.OXBOT_CAMOUFOX_BUNDLE;
const headless = process.env.OXBOT_CAMOUFOX_SMOKE_HEADLESS === "1";
const minimizedStates = headless ? [] : ["minimized"];
if (!bundle)
  throw new Error("Set OXBOT_CAMOUFOX_BUNDLE to the generated runtime bundle");
const { launchNative } = await import(
  pathToFileURL(resolve(bundle, "runtime.mjs")).href
);
async function bundleInventory(directory) {
  const result = {};
  async function walk(root, prefix = "") {
    for (const entry of (await readdir(root, { withFileTypes: true })).sort(
      (a, b) => a.name.localeCompare(b.name),
    )) {
      const file = resolve(root, entry.name),
        name = prefix + entry.name;
      if (entry.isDirectory()) await walk(file, name + "/");
      else if (entry.isSymbolicLink())
        result[name] = "link:" + (await readlink(file));
      else if (entry.isFile()) {
        const hash = createHash("sha256");
        for await (const chunk of createReadStream(file)) hash.update(chunk);
        result[name] = hash.digest("hex");
      }
    }
  }
  await walk(directory);
  return result;
}
const bundleBefore = noDownloads ? await bundleInventory(bundle) : undefined;
const temp = await mkdtemp(resolve(tmpdir(), "oxbot-camoufox-smoke-"));
const profile = resolve(temp, "profile");
let requests = [],
  runtime;
const fixture = `<!doctype html><meta charset="utf-8"><title>Oxbot local browser test</title>
<style>body{font:22px Helvetica,Arial,sans-serif;margin:35px;color:#222;background:white}p{padding:8px;border-bottom:1px solid #ddd}input,button{font:inherit;padding:8px}</style>
<h1>Weekly groceries — local test</h1><p>English: Salmon, milk, apples — $12.99</p>
<p>Français : œufs, crème fraîche</p><p>தமிழ்: வணக்கம் • हिन्दी: नमस्ते</p>
<p>中文：每周杂货 • 日本語：食料品</p><p lang="ar" dir="rtl">العربية: مرحباً</p>
<p>Emoji: 🛒 🥛 🍎 🐟 ✅</p><label>Grocery item <input aria-label="Grocery item"></label>
<button id="add">Add</button><p id="status">Ready</p>
<script>document.querySelector('#add').onclick=async()=>{await fetch('/cart',{method:'POST',body:document.querySelector('input').value});document.querySelector('#status').textContent='Added';localStorage.setItem('fixture-item','milk');};</script>`;
const server = createServer((req, res) => {
  requests.push({ method: req.method, path: req.url });
  res.setHeader("Cache-Control", "no-store");
  if (req.url === "/cart") {
    req.resume();
    res.writeHead(204).end();
  } else if (req.url === "/") {
    res.setHeader("Content-Type", "text/html; charset=utf-8");
    res.end(fixture);
  } else res.writeHead(404).end();
});
await new Promise((done) => server.listen(0, "127.0.0.1", done));
const origin = `http://127.0.0.1:${server.address().port}`;
const timeout = setTimeout(() => {
  console.error("Smoke test exceeded 120 seconds");
  process.exit(1);
}, 120000);
const out = resolve(
  process.env.OXBOT_CAMOUFOX_SMOKE_OUT ?? resolve(bundle, "../camoufox-smoke"),
);
await mkdir(out, { recursive: true });
try {
  const foreignProfile = resolve(temp, "foreign-profile");
  await mkdir(foreignProfile);
  await writeFile(
    resolve(foreignProfile, "keep.txt"),
    "another browser profile",
  );
  await assert.rejects(
    launchNative({ bundle, profile: foreignProfile }),
    /not created by this runtime/,
  );
  assert.equal(
    await readFile(resolve(foreignProfile, "keep.txt"), "utf8"),
    "another browser profile",
  );
  runtime = await launchNative({ bundle, profile, locale: "en-CA", headless });
  assert.deepEqual(await runtime.windowState(), minimizedStates);
  assert.equal(requests.length, 0, "startup must not restore a target page");
  await assert.rejects(
    launchNative({ bundle, profile }),
    /profile is already in use/,
  );
  const page = runtime.context.pages()[0];
  // The upcoming adapter must apply policy before supplying target URLs. The
  // proof permits loopback only and rejects any background page request outside it.
  const unexpected = [];
  await runtime.context.route("**/*", async (route) => {
    if (new URL(route.request().url()).origin !== origin) {
      unexpected.push(new URL(route.request().url()).origin);
      await route.abort();
    } else await route.continue();
  });
  await page.goto(origin);
  await page.locator("input").fill("milk");
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await page.locator("#status").filter({ hasText: "Added" }).waitFor();
  assert.deepEqual(await runtime.windowState(), minimizedStates);
  assert.equal(
    requests.filter((r) => r.path === "/cart" && r.method === "POST").length,
    1,
    "a cart action is sent once",
  );
  assert.deepEqual(unexpected, [], "fixture caused no external page requests");
  const identity = await page.evaluate(() => ({
    ua: navigator.userAgent,
    languages: navigator.languages,
  }));
  const measurements = await page
    .locator("p")
    .evaluateAll((items) => items.map((e) => e.getBoundingClientRect().height));
  await page.screenshot({
    path: resolve(out, "minimized.png"),
    fullPage: true,
    animations: "disabled",
  });
  await runtime.showForUser();
  assert.deepEqual(await runtime.windowState(), headless ? [] : ["maximized"]);
  await page.screenshot({
    path: resolve(out, "visible.png"),
    fullPage: true,
    animations: "disabled",
  });
  await runtime.minimize();
  const firstSaved = JSON.parse(
    await readFile(resolve(profile, ".oxbot-browser-profile.json"), "utf8"),
  );
  await runtime.close();
  runtime = undefined;
  const beforeRestart = requests.length;
  runtime = await launchNative({ bundle, profile, locale: "en-CA", headless });
  assert.deepEqual(await runtime.windowState(), minimizedStates);
  assert.equal(
    requests.length,
    beforeRestart,
    "reopening a profile must not restore the website",
  );
  const secondSaved = JSON.parse(
    await readFile(resolve(profile, ".oxbot-browser-profile.json"), "utf8"),
  );
  assert.equal(
    createHash("sha256").update(JSON.stringify(secondSaved)).digest("hex"),
    createHash("sha256").update(JSON.stringify(firstSaved)).digest("hex"),
    "a profile keeps its backend metadata between launches",
  );
  const second = runtime.context.pages()[0];
  await second.goto(origin);
  assert.equal(
    await second.evaluate(() => localStorage.getItem("fixture-item")),
    "milk",
    "site storage persists",
  );
  assert.deepEqual(
    await second.evaluate(() => ({
      ua: navigator.userAgent,
      languages: navigator.languages,
    })),
    identity,
  );
  await runtime.close();
  runtime = undefined;
  if (noDownloads)
    assert.deepEqual(
      await bundleInventory(bundle),
      bundleBefore,
      "runtime must not modify installed bundle files",
    );
  const result = {
    ok: true,
    requests,
    identity,
    measurements,
    headless,
    firstLaunchMinimized: !headless,
    restartMinimized: !headless,
    storagePersisted: true,
    nativeIdentityStable: true,
    nodeDownloadsBlocked: noDownloads,
    bundleUnchanged: noDownloads ? true : undefined,
    externalPageRequests: unexpected,
  };
  await writeFile(
    resolve(out, "result.json"),
    JSON.stringify(result, null, 2) + "\n",
  );
  console.log(
    JSON.stringify({ ok: true, output: out, requests: requests.length }),
  );
} finally {
  clearTimeout(timeout);
  await runtime?.close();
  server.closeAllConnections();
  await new Promise((done) => server.close(done));
  await rm(temp, { recursive: true, force: true });
}
