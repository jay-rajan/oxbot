// Build-time only. Fetches pinned input, removes unused fonts, and stages the
// complete runtime. Never called by the installed app or browser startup.
import { createWriteStream } from "node:fs";
import { randomUUID } from "node:crypto";
import {
  cp,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rename,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn } from "node:child_process";
import { pipeline } from "node:stream/promises";
import { Readable } from "node:stream";
import { sha256, verifyBundle } from "./assets.mjs";

const here = dirname(fileURLToPath(import.meta.url));
export const pin = JSON.parse(
  await readFile(resolve(here, "browser-pin.json"), "utf8"),
);

async function run(program, args) {
  await new Promise((done, fail) => {
    const child = spawn(program, args, {
      stdio: ["ignore", "ignore", "inherit"],
    });
    child.on("error", fail);
    child.on("exit", (code) =>
      code === 0 ? done() : fail(new Error(`${program} failed (${code})`)),
    );
  });
}

export async function verifiedArchive(file, variant, { download = true } = {}) {
  try {
    if ((await sha256(file)) === variant.sha256) return file;
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  if (!download)
    throw new Error("Camoufox archive checksum does not match the pin");
  await mkdir(dirname(file), { recursive: true });
  const part = `${file}.part-${process.pid}`;
  try {
    const url = `https://github.com/daijro/camoufox/releases/download/v${pin.version}/${variant.asset}`;
    const response = await fetch(url);
    if (!response.ok || !response.body)
      throw new Error(`Camoufox download failed (${response.status})`);
    await pipeline(
      Readable.fromWeb(response.body),
      createWriteStream(part, { flags: "wx", mode: 0o600 }),
    );
    if ((await sha256(part)) !== variant.sha256)
      throw new Error("Downloaded Camoufox failed checksum verification");
    await rename(part, file);
  } finally {
    await rm(part, { force: true });
  }
  return file;
}

export async function treeBytes(root) {
  let bytes = 0;
  for (const item of await readdir(root, { withFileTypes: true })) {
    const file = resolve(root, item.name);
    if (item.isDirectory()) bytes += await treeBytes(file);
    else if (item.isFile()) bytes += (await stat(file)).size;
  }
  return bytes;
}

export async function build({
  arch = process.arch,
  output = resolve(here, "../../target/camoufox-bundle"),
  archive,
} = {}) {
  if (process.platform !== "darwin" || !pin.variants[arch])
    throw new Error("Build on macOS for arm64 or x64");
  if (arch !== process.arch)
    throw new Error(
      "Build on the target architecture; native npm dependencies cannot be cross-copied",
    );
  const variant = pin.variants[arch];
  // Refuse to replace an arbitrary caller-supplied directory. Only an output
  // from this builder can be upgraded in place.
  try {
    await stat(output);
    const existing = JSON.parse(
      await readFile(resolve(output, "bundle.json"), "utf8"),
    );
    if (existing.schema !== 1 || existing.source !== pin.source)
      throw new Error("Output is not a generated Camoufox bundle");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    try {
      await stat(output);
      throw new Error("Refusing to replace an unrelated output directory");
    } catch (missing) {
      if (missing.code !== "ENOENT") throw missing;
    }
  }
  const file =
    archive ?? resolve(here, "../../target/tools-cache", variant.asset);
  await verifiedArchive(file, variant, { download: !archive });
  await mkdir(dirname(output), { recursive: true });
  const stage = await mkdtemp(resolve(dirname(output), ".camoufox-stage-"));
  try {
    await mkdir(resolve(stage, "browser"));
    await run("/usr/bin/ditto", ["-x", "-k", file, resolve(stage, "browser")]);
    await rm(resolve(stage, "browser", pin.omit), {
      recursive: true,
      force: true,
    });
    for (const name of [
      "runtime.mjs",
      "window-bridge.mjs",
      "assets.mjs",
      "package.json",
      "package-lock.json",
      "browser-pin.json",
      "README.md",
      "window-addon",
    ])
      await cp(resolve(here, name), resolve(stage, name), { recursive: true });
    // Install only the locked production graph in the staging directory.
    // Copying checkout node_modules can include extraneous packages or leave
    // symlinks pointing back into the developer's machine.
    await run("npm", [
      "ci",
      "--prefix",
      stage,
      "--omit=dev",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
    ]);
    const executable = resolve(stage, pin.executable);
    const manifest = {
      schema: 1,
      platform: "darwin",
      arch,
      version: pin.version,
      source: pin.source,
      sourceArchiveSha256: variant.sha256,
      executable: pin.executable,
      executableSha256: await sha256(executable),
      fonts: "native-system",
      configuration: "native",
      releaseReady: false,
    };
    await writeFile(
      resolve(stage, "bundle.json"),
      JSON.stringify(manifest, null, 2) + "\n",
    );
    await verifyBundle(stage, { platform: "darwin", arch });
    const sizes = {
      browser: await treeBytes(resolve(stage, "browser")),
      dependencies: await treeBytes(resolve(stage, "node_modules")),
      total: await treeBytes(stage),
    };
    await writeFile(
      resolve(stage, "sizes.json"),
      JSON.stringify(sizes, null, 2) + "\n",
    );
    // Replace only generated output. A staging failure leaves a previous build intact.
    const old = resolve(dirname(output), `.camoufox-previous-${randomUUID()}`);
    try {
      await rename(output, old);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    try {
      await rename(stage, output);
    } catch (error) {
      try {
        await rename(old, output);
      } catch {}
      throw error;
    }
    await rm(old, { recursive: true, force: true });
    console.log(JSON.stringify({ output, ...sizes }));
    return { output, sizes };
  } finally {
    await rm(stage, { recursive: true, force: true });
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  const args = process.argv.slice(2),
    options = {};
  for (let i = 0; i < args.length; i += 2) {
    const key = { "--arch": "arch", "--out": "output", "--archive": "archive" }[
      args[i]
    ];
    if (!key || !args[i + 1])
      throw new Error(
        "Usage: node build.mjs [--arch arm64|x64] [--out DIR] [--archive PINNED_ZIP]",
      );
    options[key] = key === "arch" ? args[i + 1] : resolve(args[i + 1]);
  }
  await build(options);
}
