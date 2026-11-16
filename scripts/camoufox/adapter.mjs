import { randomBytes, randomUUID } from "node:crypto";
import { mkdir, realpath, writeFile, readFile } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";
import { launchNative } from "./runtime.mjs";
import { workspaceFile } from "./files.mjs";
import { names } from "./tool-specs.mjs";
import { toDevtools } from "./snapshot.mjs";

const factsSource = await readFile(
  new URL("./element-facts.js", import.meta.url),
  "utf8",
);
const factsFunction = new Function(
  "element",
  `return (${factsSource}).call(element)`,
);
const interactive = new Set([
  "button",
  "link",
  "textbox",
  "searchbox",
  "combobox",
  "checkbox",
  "radio",
  "option",
  "switch",
  "menuitem",
  "tab",
]);
// Checked by a click: a styled one is often covered by its own drawing.
const toggles = new Set(["checkbox", "radio", "switch", "menuitemcheckbox", "menuitemradio"]);
/// The longest the page may take to settle after a click or key press,
/// for its network and then for its content.
const SETTLE_MS = 2000;
const clean = (value) =>
  String(value ?? "")
    .replace(/[\r\n\u2028\u2029]/g, " ")
    .slice(0, 2000);
// Must match oxbot-core's hub/elements.rs SNAPSHOT_TOOLS.
const SNAPSHOT_RESULTS = new Set([
  "take_snapshot",
  "wait_for",
  "click",
  "drag",
  "fill",
  "fill_form",
  "hover",
  "press_key",
  "upload_file",
]);
const unavailable =
  "This operation is unavailable while you are using the browser";
// Bytes a request sends, estimated like the Chrome guard did: address,
// headers and body (a bigger body counts as this much).
const MAX_POST_DATA = 65536;
const requestBytes = (request) => {
  let bytes = request.url().length + request.method().length;
  for (const [key, value] of Object.entries(request.headers()))
    bytes += key.length + String(value).length + 4;
  const body = request.postDataBuffer();
  return bytes + Math.min(body ? body.length : 0, MAX_POST_DATA);
};
const escapeRegExp = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

export class Adapter {
  static async start(options, onEvent = () => {}) {
    const runtime = await launchNative(options);
    const adapter = new Adapter(runtime, options, onEvent);
    // The startup tab may be the private launcher: bots get a page of their own.
    if (adapter.pages.size === 0) adapter.track(await runtime.newPage());
    return adapter;
  }
  constructor(runtime, options, onEvent = () => {}) {
    this.onEvent = onEvent;
    this.runtime = runtime;
    this.context = runtime.context;
    this.options = options;
    this.paused = false;
    this.pages = new Map();
    this.ids = new WeakMap();
    this.loading = new WeakMap();
    this.refs = new Map();
    this.nextPage = 1;
    this.nextUid = 1;
    this.focusRef = undefined;
    this.generation = randomBytes(8).toString("hex");
    this.resetNote = true;
    this.signInPages = [];
    this.active = undefined;
    this.downloads = undefined;
    this.workspace = undefined;
    this.dialog = undefined;
    this.closing = false;
    for (const page of this.context.pages()) this.track(page);
    this.context.on("page", (page) => this.track(page));
    // Site activity for Settings -> Activity. Nothing is counted while the
    // person has the browser, and no headers, bodies or cookies leave Node:
    // only the address (the hub keeps just its site) and byte counts.
    this.context.on("request", (request) => {
      const tab = this.tabOf(request);
      if (tab === undefined) return;
      let top = false;
      try {
        top =
          request.isNavigationRequest() &&
          request.frame() === request.frame().page().mainFrame();
      } catch {}
      this.onEvent({
        event: "net",
        kind: "request",
        tab,
        url: request.url().slice(0, 2000),
        top,
        redirect: !!request.redirectedFrom(),
        up: requestBytes(request),
      });
    });
    this.context.on("requestfinished", async (request) => {
      const tab = this.tabOf(request);
      if (tab === undefined) return;
      const sizes = await request.sizes().catch(() => undefined);
      if (!sizes) return;
      this.onEvent({
        event: "net",
        kind: "received",
        tab,
        bytes: Math.max(0, sizes.responseBodySize + sizes.responseHeadersSize),
      });
    });
  }
  tabOf(request) {
    if (this.paused || this.closing) return undefined;
    try {
      const id = this.ids.get(request.frame().page());
      return id === undefined ? undefined : String(id);
    } catch {
      // Service-worker requests belong to no tab.
      return undefined;
    }
  }
  track(page) {
    if (page === this.runtime.launcher) return undefined;
    if (this.ids.has(page)) return this.ids.get(page);
    const id = this.nextPage++;
    this.ids.set(page, id);
    this.pages.set(id, page);
    this.active ??= id;
    // Requests still loading, so a click's page is read once they're done.
    const loading = new Set();
    this.loading.set(page, loading);
    page.on("request", (request) => loading.add(request));
    page.on("requestfinished", (request) => loading.delete(request));
    page.on("requestfailed", (request) => loading.delete(request));
    page.on("framenavigated", (frame) => {
      if (frame === page.mainFrame()) this.invalidate(page);
    });
    page.on("close", () => {
      this.invalidate(page);
      this.pages.delete(id);
      this.onEvent({ event: "net", kind: "gone", tab: String(id) });
      // The person closed the sign-in page(s): the sign-in is over, as
      // closing Chrome's sign-in window was.
      const at = this.signInPages.indexOf(page);
      if (at >= 0) {
        this.signInPages.splice(at, 1);
        if (this.paused && !this.closing && this.signInPages.length === 0)
          void this.endSignIn().then(() => this.onEvent({ event: "resumed" }));
      }
      if (this.active === id) this.active = this.pages.keys().next().value;
    });
    page.on("dialog", (dialog) => {
      this.dialog = dialog;
    });
    page.on("download", async (download) => {
      if (this.paused || !this.downloads) {
        await download.cancel().catch(() => {});
        return;
      }
      const name =
        basename(download.suggestedFilename()).replace(
          /[^\p{L}\p{N}._-]/gu,
          "_",
        ) || "download";
      try {
        const file = await workspaceFile(
          this.workspace,
          resolve(this.downloads, `${randomUUID()}-${name}`),
        );
        await download.saveAs(file);
      } catch {
        await download.cancel().catch(() => {});
      }
    });
    return id;
  }
  invalidate(page) {
    for (const [uid, entry] of this.refs)
      if (entry.page === page) this.refs.delete(uid);
  }
  available() {
    if (this.paused) throw new Error(unavailable);
  }
  // The tab showing `url`, else the one in use, as the Chrome guard picked:
  // callers check the address the browser reports, never the one asked for.
  page(url) {
    const page =
      (url && Array.from(this.pages.values()).find((p) => p.url() === url)) ||
      this.pages.get(this.active);
    if (!page || page.isClosed())
      throw new Error("The page is no longer open; take a fresh snapshot");
    return page;
  }
  async pagesText() {
    const rows = [];
    for (const [id, page] of this.pages)
      rows.push(
        `${id}: ${clean(await page.title().catch(() => ""))} (${clean(page.url())})${id === this.active ? " [selected]" : ""}`,
      );
    return `## Pages\n${rows.join("\n")}`;
  }
  // After a click or a key press, waits for the page to settle: its
  // network quiet, then its content unchanged for 3/4 of a second (a
  // booking calendar fills in after the click). At most SETTLE_MS each,
  // so a busy page still answers.
  async settle() {
    const page = this.pages.get(this.active);
    if (!page || page.isClosed()) return;
    const loading = this.loading.get(page) ?? new Set();
    // Pages that never stop loading (ads, live chat) still answer.
    const deadline = Date.now() + SETTLE_MS;
    while (loading.size > 0 && Date.now() < deadline)
      await page.waitForTimeout(100).catch(() => {});
    const quiet = Date.now() + SETTLE_MS;
    let last = -1,
      still = 0;
    while (Date.now() < quiet) {
      const size = await page
        .evaluate(() => document.body?.innerHTML.length ?? 0)
        .catch(() => -1);
      if (size > 0 && size === last && loading.size === 0) {
        if (++still >= 3) return;
      } else still = 0;
      last = size;
      await page.waitForTimeout(250).catch(() => {});
    }
  }
  async snapshot() {
    this.available();
    const page = this.page();
    // Playwright's AI snapshot has browser-managed aria references,
    // including frame references. No page-writable uid attributes are used.
    const aria = await page.ariaSnapshot({ mode: "ai" });
    const { lines, refs } = toDevtools(aria, {
      title: await page.title(),
      url: page.url(),
      uid: (n) => (n === "root" ? `${this.generation}_root` : `${this.generation}_${this.nextUid++}`),
    });
    for (const { uid, ref, role, name } of refs) {
      // Facts are read when the element is first used, not here: a big
      // page has hundreds of controls and one page call each.
      const entry = { page, ref, role, name, interactive: interactive.has(role) };
      if (entry.interactive && this.focusRef?.page === page && this.focusRef.ref === ref)
        this.focusUid = uid;
      this.refs.set(uid, entry);
    }
    if (this.refs.size > 20000) {
      const keys = Array.from(this.refs.keys());
      for (const key of keys.slice(0, this.refs.size - 20000))
        this.refs.delete(key);
    }
    return `${await this.pagesText()}\n## Latest page snapshot\n${lines.join("\n")}`;
  }
  // `anyRole`: a click or hover may aim at anything the snapshot named
  // (sites make headings, cards and pictures clickable); typing needs a
  // field.
  async element(uid, { anyRole = false } = {}) {
    const entry = this.refs.get(uid);
    if (!entry || entry.page !== this.page())
      throw new Error("Stale element uid; take a fresh snapshot");
    const locator = entry.page.locator(`aria-ref=${entry.ref}`);
    const expected = entry.page.getByRole(entry.role, {
      name: entry.name,
      exact: true,
    });
    const checked = locator.and(expected);
    if ((await checked.count()) !== 1)
      throw new Error("The element changed; take a fresh snapshot");
    if (!entry.interactive && !anyRole)
      throw new Error("Use an interactive element from a fresh snapshot");
    // The facts at first use; a later use must find them unchanged.
    const facts = await checked.evaluate(factsFunction);
    if (entry.facts === undefined) entry.facts = facts;
    else if (JSON.stringify(facts) !== JSON.stringify(entry.facts))
      throw new Error("The form changed; take a fresh snapshot before acting");
    return checked;
  }
  // A checkbox, radio or switch. A styled one is often covered by its own
  // drawing, which Playwright won't click through: then click where it
  // is, as a person would, and if that didn't change it, click the
  // element itself.
  async toggle(locator) {
    try {
      await locator.click({ timeout: 4000 });
      return;
    } catch (error) {
      if (!/Timeout|intercepts pointer events|not visible|outside of the viewport/.test(String(error?.message)))
        throw error;
    }
    const before = await locator.isChecked().catch(() => undefined);
    await locator.click({ force: true, timeout: 4000 }).catch(() => {});
    const after = await locator.isChecked().catch(() => undefined);
    if (before === undefined || before === after)
      await locator.evaluate((element) => element.click());
  }
  async fill(uid, value) {
    const locator = await this.element(uid);
    const type = await locator.getAttribute("type");
    const autocomplete = await locator.getAttribute("autocomplete");
    if (
      type === "password" ||
      /^(?:cc-|one-time-code)/i.test(autocomplete ?? "")
    )
      throw new Error(
        "Use the sign-in handoff for passwords, codes, and payment details",
      );
    this.focusRef = this.refs.get(uid);
    this.focusUid = uid;
    if (await locator.evaluate((e) => e.tagName.toLowerCase() === "select"))
      await locator.selectOption({ label: value });
    else await locator.fill(value);
  }
  async tool(name, args, workspace) {
    this.available();
    if (!names.has(name)) throw new Error("Unsupported browser tool");
    if (!this.workspace || (await realpath(workspace)) !== (await realpath(this.workspace)))
      throw new Error(
        "This tool session does not own the active bot workspace",
      );
    const page = this.page();
    switch (name) {
      case "navigate_page": {
        const type = args.type ?? "url";
        if (type === "url") {
          const url = new URL(args.url);
          if (
            !["http:", "https:", "about:"].includes(url.protocol) ||
            (url.protocol === "about:" && url.href !== "about:blank")
          )
            throw new Error("Only web URLs and about:blank are allowed");
          await page.goto(url.href, { waitUntil: "domcontentloaded" });
        } else if (type === "back")
          await page.goBack({ waitUntil: "domcontentloaded" });
        else if (type === "forward")
          await page.goForward({ waitUntil: "domcontentloaded" });
        else if (type === "reload")
          await page.reload({ waitUntil: "domcontentloaded" });
        else throw new Error("Unknown navigation type");
        break;
      }
      case "new_page": {
        const url = new URL(args.url);
        if (!["http:", "https:"].includes(url.protocol))
          throw new Error("Only web URLs are allowed");
        const next = await this.runtime.newPage();
        this.active = this.track(next);
        await this.closeOldPages();
        await next.goto(url.href, { waitUntil: "domcontentloaded" });
        break;
      }
      case "select_page":
        if (!this.pages.has(args.pageId)) throw new Error("No such page");
        this.active = args.pageId;
        break;
      case "close_page": {
        const target = this.pages.get(args.pageId);
        if (!target) throw new Error("No such page");
        if (this.pages.size === 1) this.track(await this.runtime.newPage());
        await target.close();
        break;
      }
      case "list_pages":
        return { text: await this.pagesText() };
      case "take_snapshot":
        break;
      case "click": {
        const locator = await this.element(args.uid, { anyRole: true });
        const entry = this.refs.get(args.uid);
        this.focusRef = entry;
        this.focusUid = args.uid;
        if (args.dblClick) await locator.dblclick();
        else if (toggles.has(entry.role)) await this.toggle(locator);
        else await locator.click();
        await this.settle();
        break;
      }
      case "hover":
        await (await this.element(args.uid, { anyRole: true })).hover();
        break;
      case "fill":
        await this.fill(args.uid, args.value);
        break;
      case "fill_form":
        for (const element of args.elements)
          await this.fill(element.uid, element.value);
        break;
      case "type_text":
        throw new Error(
          "Use fill with a fresh input uid; typing into an unknown focus is refused",
        );
      case "press_key":
        if (!this.focusUid)
          throw new Error(
            "Fill or click the intended control before pressing a key",
          );
        const target = await this.element(this.focusUid);
        if ((await target.getAttribute("type")) === "password")
          throw new Error("Use the sign-in handoff for secret fields");
        await target.press(args.key);
        await this.settle();
        break;
      case "drag":
        await (
          await this.element(args.from_uid)
        ).dragTo(await this.element(args.to_uid));
        break;
      case "upload_file": {
        const paths = args.filePaths ?? [args.filePath];
        if (!paths.length || paths.some((p) => typeof p !== "string"))
          throw new Error("Missing upload file");
        const safe = await Promise.all(
          paths.map((file) =>
            workspaceFile(this.workspace, file, { existing: true }),
          ),
        );
        await (await this.element(args.uid)).setInputFiles(safe);
        break;
      }
      case "handle_dialog":
        if (!this.dialog) throw new Error("No dialog is open");
        if (args.action === "accept") await this.dialog.accept(args.promptText);
        else await this.dialog.dismiss();
        this.dialog = undefined;
        break;
      case "wait_for":
        await page.waitForFunction(
          (texts) =>
            texts.some((text) =>
              (document.body?.innerText ?? "").includes(text),
            ),
          args.text,
          { timeout: Math.min(Math.max(args.timeout ?? 10000, 1), 30000) },
        );
        break;
      case "take_screenshot": {
        const image = await page.screenshot({
          fullPage: args.fullPage ?? false,
        });
        if (args.filePath)
          await writeFile(
            await workspaceFile(this.workspace, args.filePath),
            image,
          );
        return {
          text: await this.pagesText(),
          image: image.toString("base64"),
        };
      }
    }
    // Only the tools whose results carry a snapshot in chrome-devtools-mcp
    // (oxbot-core's SNAPSHOT_TOOLS) carry one here: the hub learns element
    // names from exactly those, so a uid seen anywhere else would reach an
    // approval card unnamed. Navigation answers with the page list.
    const result = SNAPSHOT_RESULTS.has(name)
      ? await this.snapshot()
      : `${await this.pagesText()}\nTake a snapshot to see the page.`;
    const prefix = this.resetNote
      ? "Note: the browser was restarted or reconnected\n"
      : "";
    this.resetNote = false;
    return { text: prefix + result };
  }
  // A bot keeps at most MAX_PAGES tabs: the oldest it isn't on closes
  // (page ids grow, so the smallest are the oldest).
  async closeOldPages() {
    const MAX_PAGES = 3;
    const old = [...this.pages.entries()].filter(
      ([id, page]) => id !== this.active && !this.signInPages.includes(page),
    );
    while (this.pages.size > MAX_PAGES && old.length) {
      const [, page] = old.shift();
      await page.close().catch(() => {});
    }
  }
  // Back to automation: what bots read before is stale.
  async endSignIn() {
    this.refs.clear();
    this.resetNote = true;
    await this.runtime.minimize().catch(() => {});
    this.paused = false;
  }
  async admin(method, params = {}) {
    if (method === "ping") return true;
    if (method === "shutdown") {
      this.closing = true;
      await this.runtime.close();
      return true;
    }
    if (method === "open_window") {
      this.paused = true;
      this.refs.clear();
      const page = await this.runtime.newPage();
      this.track(page);
      this.signInPages.push(page);
      await this.runtime.showForUser();
      await page.bringToFront();
      await page
        .goto(params.url, { waitUntil: "domcontentloaded" })
        .catch(() => {});
      return true;
    }
    if (method === "close_window") {
      const pages = this.signInPages;
      this.signInPages = [];
      for (const page of pages) {
        if (this.pages.size === 1) this.track(await this.runtime.newPage());
        await page.close().catch(() => {});
      }
      await this.endSignIn();
      return true;
    }
    this.available();
    switch (method) {
      case "downloads": {
        await mkdir(params.dir, { recursive: true, mode: 0o700 });
        this.downloads = await realpath(params.dir);
        this.workspace = resolve(dirname(params.dir));
        return true;
      }
      case "reset_tabs": {
        const old = [...this.pages.values()],
          next = await this.runtime.newPage();
        this.active = this.track(next);
        for (const page of old) await page.close().catch(() => {});
        this.refs.clear();
        this.resetNote = true;
        return true;
      }
      case "page_url":
        return this.page(params.page_url).url();
      case "screenshot":
        return (await this.page(params.page_url).screenshot()).toString(
          "base64",
        );
      case "inspect":
        return this.page(params.page_url).evaluate(params.expression);
      case "describe": {
        const page = this.page(params.page_url);
        const locator = page.getByRole(params.role, {
          name: params.name,
          exact: true,
        });
        const result = [];
        const count = Math.min(await locator.count(), 8);
        for (let i = 0; i < count; i++)
          result.push(
            await locator
              .nth(i)
              .evaluate(
                new Function(
                  "element",
                  `return (${params.function}).call(element)`,
                ),
              ),
          );
        return result;
      }
      case "uid_check": {
        // The element a uid names, as the browser's own snapshot recorded
        // it: gone (the page changed or another tab is in use), or its
        // role and name and whether the live page still shows it so.
        const entry = this.refs.get(params.uid);
        if (!entry || entry.page.isClosed() || entry.page !== this.page())
          return { live: false };
        const shown = await entry.page
          .locator(`aria-ref=${entry.ref}`)
          .and(entry.page.getByRole(entry.role, { name: entry.name, exact: true }))
          .count();
        return { live: true, role: entry.role, name: entry.name, shown: shown === 1 };
      }
      case "count_named":
        return this.page(params.page_url)
          .getByRole(params.role, { name: params.name, exact: true })
          .count();
      case "cookies":
        return this.context.cookies();
      case "add_cookies": {
        // Firefox silently drops a cookie that expires more than 400 days
        // out (the cookie lifetime cap), so a longer one is shortened.
        const latest = Math.floor(Date.now() / 1000) + 399 * 24 * 3600;
        const key = (c) => `${c.name}\u0000${c.domain}\u0000${c.path ?? "/"}`;
        const wanted = new Set();
        await this.context.addCookies(
          params.cookies.map((cookie) => {
            const clean = { ...cookie };
            delete clean.sourcePort;
            delete clean.sourceScheme;
            delete clean.priority;
            delete clean.session;
            delete clean.size;
            delete clean.partitionKey;
            if (typeof clean.expires === "number" && clean.expires > latest)
              clean.expires = latest;
            wanted.add(key(clean));
            return clean;
          }),
        );
        // How many the browser kept.
        return (await this.context.cookies()).filter((c) => wanted.has(key(c)))
          .length;
      }
      case "sign_out_site": {
        // The site and its subdomains: cookies, then what its open pages
        // stored. Other sites stay signed in.
        const site = String(params.site ?? "").toLowerCase();
        if (!/^[a-z0-9.-]+$/.test(site) || !site.includes("."))
          throw new Error("Not a site name");
        const ofSite = (host) => {
          host = host.replace(/^\./, "");
          return host === site || host.endsWith(`.${site}`);
        };
        const gone = (await this.context.cookies()).filter((c) =>
          ofSite(c.domain),
        );
        await this.context.clearCookies({
          domain: new RegExp(`(^|\\.)${escapeRegExp(site)}$`),
        });
        for (const page of this.pages.values()) {
          let host = "";
          try {
            host = new URL(page.url()).hostname;
          } catch {}
          if (!ofSite(host)) continue;
          await page
            .evaluate(async () => {
              localStorage.clear();
              sessionStorage.clear();
              for (const db of (await indexedDB.databases?.()) ?? [])
                if (db.name) indexedDB.deleteDatabase(db.name);
              if (globalThis.caches)
                for (const key of await caches.keys()) await caches.delete(key);
            })
            .catch(() => {});
        }
        return gone.length;
      }
      case "tool":
        return this.tool(params.name, params.args ?? {}, params.workspace);
      default:
        throw new Error("Unsupported browser administration command");
    }
  }
}
