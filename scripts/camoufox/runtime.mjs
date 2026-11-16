// Lifecycle primitive for the upcoming Rust browser adapter. Not an MCP server
// and not wired into live bots: policy/isolated-read parity is a release gate.
import { firefox } from "playwright-core";
import {
  cp,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { verifyBundle } from "./assets.mjs";
import { windowBridge } from "./window-bridge.mjs";

const here = dirname(fileURLToPath(import.meta.url));

export function nativeOptions({
  executable,
  addons,
  locale,
  headless = false,
  proxy,
  environment = process.env,
}) {
  // No fingerprint generator or saved synthetic identity. Firefox reports the
  // real OS/fonts/GPU. Inherited Camoufox config cannot override that choice.
  const env = Object.fromEntries(
    Object.entries(environment).filter(
      ([key]) => !key.startsWith("CAMOU_") && !key.startsWith("CAMOUFOX_"),
    ),
  );
  env.CAMOU_CONFIG_1 = JSON.stringify({ addons });
  return {
    executablePath: executable,
    headless,
    viewport: null,
    locale,
    env,
    // End-to-end tests only: the test site answers for every host.
    ...(proxy ? { proxy: { server: proxy } } : {}),
    firefoxUserPrefs: {
      // Browser updates are distributed together with the signed Oxbot app.
      "app.update.auto": false,
      "app.update.enabled": false,
      // Opening the helper must never re-load websites from a previous run.
      "browser.startup.page": 0,
      "browser.sessionstore.resume_from_crash": false,
      "browser.sessionstore.resume_session_once": false,
      "browser.sessionstore.max_resumed_crashes": 0,
    },
  };
}

// A helper that was killed (or a Mac that lost power) leaves its lock behind.
// The lock names its owner's process: a lock whose owner is gone is cleared
// once; a live owner keeps the profile.
const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
};
async function takeLock(lock) {
  for (let attempt = 0; ; attempt++) {
    try {
      await mkdir(lock, { mode: 0o700 });
      return;
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
    }
    let pid;
    try {
      pid = JSON.parse(await readFile(resolve(lock, "owner.json"), "utf8")).pid;
    } catch {}
    if (attempt > 0 || (Number.isInteger(pid) && pid > 0 && alive(pid)))
      throw new Error(
        "This browser profile is already in use or has an uncleared runtime lock",
      );
    // No owner file at all: the owner died between taking the lock and
    // writing it, or the lock predates owner files.
    await rm(lock, { recursive: true, force: true });
  }
}

export async function launchNative({
  bundle,
  profile,
  locale = Intl.DateTimeFormat().resolvedOptions().locale,
  headless = false,
  proxy,
}) {
  const { executable, addon } = await verifyBundle(bundle);
  const profileDir = resolve(profile),
    rel = relative(resolve(bundle), profileDir);
  if (
    rel === "" ||
    (rel !== ".." && !rel.startsWith(`..${sep}`) && !rel.startsWith(sep))
  )
    throw new Error(
      "The writable profile must live outside the application bundle",
    );
  await mkdir(profileDir, { recursive: true, mode: 0o700 });
  const lock = resolve(profileDir, ".oxbot-runtime-lock");
  await takeLock(lock);
  // However this process ends, it must not leave its lock behind: a later
  // start would refuse the profile while anything still answers to its pid.
  const dropLock = () => rmSync(lock, { recursive: true, force: true });
  process.once("exit", dropLock);
  let context, bridge, addonRoot, cleanupPromise;
  const cleanup = () =>
    (cleanupPromise ??= (async () => {
      try {
        await bridge?.close();
        if (addonRoot) await rm(addonRoot, { recursive: true, force: true });
      } finally {
        process.off("exit", dropLock);
        dropLock();
      }
    })());
  try {
    await writeFile(
      resolve(lock, "owner.json"),
      JSON.stringify({ pid: process.pid }),
      { mode: 0o600 },
    );
    const profileFile = resolve(profileDir, ".oxbot-browser-profile.json");
    const metadata = {
      schema: 1,
      platform: process.platform,
      arch: process.arch,
      backend: "camoufox-native",
    };
    try {
      const saved = JSON.parse(await readFile(profileFile, "utf8"));
      if (JSON.stringify(saved) !== JSON.stringify(metadata))
        throw new Error(
          "Saved browser profile is not compatible with this Mac",
        );
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
      const files = await readdir(profileDir);
      if (files.some((name) => name !== ".oxbot-runtime-lock"))
        throw new Error(
          "Refusing a nonempty profile not created by this runtime",
        );
    }
    // Mark a newly owned profile before starting Firefox, so a failed blank
    // startup can be retried without mistaking its files for another browser.
    await writeFile(profileFile + ".part", JSON.stringify(metadata), {
      mode: 0o600,
    });
    await rename(profileFile + ".part", profileFile);
    const addons = [addon];
    if (!headless) {
      bridge = await windowBridge();
      addonRoot = await mkdtemp(resolve(tmpdir(), "oxbot-window-"));
      const windowAddon = resolve(addonRoot, "addon");
      await cp(resolve(here, "window-addon"), windowAddon, { recursive: true });
      await writeFile(
        resolve(windowAddon, "bridge.json"),
        JSON.stringify({ endpoint: bridge.endpoint }),
        { mode: 0o600 },
      );
      addons.push(windowAddon);
    }
    const options = nativeOptions({
      executable,
      addons,
      locale,
      headless,
      proxy,
    });
    context = await firefox.launchPersistentContext(profileDir, options);
    context.setDefaultTimeout(10000);
    context.setDefaultNavigationTimeout(30000);
    // No target URL is accepted at launch. Startup is blank and must confirm
    // minimized window state before the caller receives the context.
    if (context.pages().some((page) => page.url() !== "about:blank"))
      throw new Error("Browser restored an unexpected page during startup");
    if (bridge) {
      const states = await bridge.setState("minimized");
      if (states.some((state) => state !== "minimized"))
        throw new Error("Browser did not minimize; navigation is refused");
    }
    context.on("close", () => {
      void cleanup().catch(() => {});
    });
    // Firefox opens every Playwright page as a window of its own. oxbot keeps
    // its startup tab as a private launcher instead: a click on its noopener
    // link opens a blank tab in the same window. Only oxbot writes this
    // about:blank page; no website runs in it.
    let launcher;
    if (bridge) {
      launcher = context.pages()[0];
      await launcher.setContent(
        '<title>oxbot</title><a id="oxbot-new-tab" href="about:blank" target="_blank" rel="noopener">oxbot</a>',
      );
    }
    return {
      // Internal adapter primitive only. Do not expose raw Playwright context
      // methods to a model; the Rust gate must mediate browser tools first.
      context,
      async windowState() {
        return bridge ? bridge.setState("status") : [];
      },
      // The private launcher tab, which is never a bot's page.
      launcher,
      // A new page, as a tab of the browser's window when it has one.
      async newPage() {
        if (!launcher || launcher.isClosed()) return context.newPage();
        const opened = context.waitForEvent("page", {
          predicate: (page) => page !== launcher,
          timeout: 10000,
        });
        // A failed click leaves this wait to fail later: never unhandled,
        // or Node ends the whole browser.
        opened.catch(() => {});
        try {
          await launcher.locator("#oxbot-new-tab").click();
          return await opened;
        } catch {
          // No tab from the launcher: a page of its own will do.
          return context.newPage();
        }
      },
      async showForUser() {
        if (bridge) await bridge.setState("normal");
      },
      async minimize() {
        if (bridge) await bridge.setState("minimized");
      },
      async close() {
        try {
          await context.close();
        } finally {
          await cleanup();
        }
      },
    };
  } catch (error) {
    try {
      await context?.close();
    } finally {
      await cleanup();
    }
    throw error;
  }
}
