import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  rm,
  writeFile,
  readFile,
  mkdir,
  readdir,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { inside, sha256, verifyBundle } from "../assets.mjs";
import { verifiedArchive, build } from "../build.mjs";
import { nativeOptions } from "../runtime.mjs";

async function temporary(t) {
  const root = await mkdtemp(resolve(tmpdir(), "oxbot-assets-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}

test("archive cache accepts only the pinned bytes; supplied corrupt input never downloads", async (t) => {
  const root = await temporary(t),
    file = resolve(root, "input.zip");
  await writeFile(file, "verified input");
  const variant = { sha256: await sha256(file) };
  assert.equal(await verifiedArchive(file, variant, { download: false }), file);
  await writeFile(file, "corrupt input");
  await assert.rejects(
    verifiedArchive(file, variant, { download: false }),
    /checksum/,
  );
  assert.deepEqual(await readdir(root), ["input.zip"]);
});

test("bundle paths cannot escape to a user profile or another app", () => {
  assert.throws(() => inside("/bundle", "../profile"), /escapes/);
  assert.throws(() => inside("/bundle", "/other/app"), /relative/);
  assert.equal(inside("/bundle", "browser/engine"), "/bundle/browser/engine");
});

test("missing bundle preflight neither installs nor creates assets", async (t) => {
  const root = await temporary(t);
  await assert.rejects(verifyBundle(root), /ENOENT/);
  assert.deepEqual(await readdir(root), []);
});

test("wrong architecture and modified executable fail before any browser launches", async (t) => {
  const root = await temporary(t);
  const executable = resolve(root, "engine");
  await writeFile(executable, "browser");
  const manifest = {
    schema: 1,
    configuration: "native",
    platform: "darwin",
    arch: "arm64",
    executable: "engine",
    executableSha256: await sha256(executable),
  };
  await writeFile(resolve(root, "bundle.json"), JSON.stringify(manifest));
  await assert.rejects(
    verifyBundle(root, { platform: "darwin", arch: "x64" }),
    /platform\/architecture/,
  );
  await writeFile(executable, "tampered");
  await assert.rejects(
    verifyBundle(root, { platform: "darwin", arch: "arm64" }),
    /damaged/,
  );
});

test("native options reject inherited synthetic identity and do not weaken TLS", () => {
  const options = nativeOptions({
    executable: "/bundle/browser",
    addons: ["/private/window-addon"],
    locale: "en-CA",
    environment: {
      PATH: "/usr/bin",
      CAMOU_CONFIG_1: "fake identity",
      CAMOU_CONFIG_2: "more",
      CAMOUFOX_EXECUTABLE_PATH: "/other/browser",
    },
  });
  assert.deepEqual(JSON.parse(options.env.CAMOU_CONFIG_1), {
    addons: ["/private/window-addon"],
  });
  assert.equal(options.env.CAMOU_CONFIG_2, undefined);
  assert.equal(options.env.CAMOUFOX_EXECUTABLE_PATH, undefined);
  assert.equal(options.env.PATH, "/usr/bin");
  assert.equal(options.headless, false);
  assert.equal(options.ignoreHTTPSErrors, undefined);
  assert.equal(
    options.firefoxUserPrefs["browser.sessionstore.resume_from_crash"],
    false,
  );
});

test(
  "builder preserves an unrelated output directory",
  { skip: process.platform !== "darwin" },
  async (t) => {
    const root = await temporary(t);
    await writeFile(resolve(root, "keep.txt"), "user file");
    await assert.rejects(build({ output: root }), /unrelated/);
    assert.equal(
      await readFile(resolve(root, "keep.txt"), "utf8"),
      "user file",
    );
  },
);

test("a full font bundle cannot slip into the reduced native package", async (t) => {
  const root = await temporary(t);
  const executable = resolve(root, "engine");
  await writeFile(executable, "browser");
  await writeFile(
    resolve(root, "bundle.json"),
    JSON.stringify({
      schema: 1,
      configuration: "native",
      platform: "darwin",
      arch: "arm64",
      executable: "engine",
      executableSha256: await sha256(executable),
    }),
  );
  await mkdir(resolve(root, "browser/Camoufox.app/Contents/Resources/fonts"), {
    recursive: true,
  });
  await assert.rejects(
    verifyBundle(root, { platform: "darwin", arch: "arm64" }),
    /Unexpected full font bundle/,
  );
});
