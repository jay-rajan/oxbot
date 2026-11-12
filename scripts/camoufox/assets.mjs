// Shared by the build tool and runtime preflight. No downloads happen here.
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";

export async function sha256(file) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest("hex");
}

export function inside(root, name) {
  if (isAbsolute(name))
    throw new Error("Bundle asset must use a relative path");
  const file = resolve(root, name),
    rel = relative(resolve(root), file);
  if (rel === ".." || rel.startsWith(`..${sep}`))
    throw new Error("Bundle asset escapes its root");
  return file;
}

export async function verifyBundle(
  root,
  { platform = process.platform, arch = process.arch } = {},
) {
  const manifest = JSON.parse(
    await readFile(resolve(root, "bundle.json"), "utf8"),
  );
  if (
    manifest.schema !== 1 ||
    manifest.platform !== platform ||
    manifest.arch !== arch
  )
    throw new Error(
      "Camoufox bundle does not match this platform/architecture",
    );
  if (platform !== "darwin")
    throw new Error("This Camoufox package is tested only for macOS");
  if (manifest.configuration !== "native")
    throw new Error("Camoufox bundle is not configured for native operation");
  const executable = inside(root, manifest.executable);
  if (
    !(await stat(executable)).isFile() ||
    (await sha256(executable)) !== manifest.executableSha256
  )
    throw new Error("Bundled Camoufox executable is missing or damaged");
  const fonts = inside(root, "browser/Camoufox.app/Contents/Resources/fonts");
  try {
    await stat(fonts);
    throw new Error("Unexpected full font bundle in native package");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const addon = inside(
    root,
    "browser/Camoufox.app/Contents/Resources/addons/UBO",
  );
  await readFile(resolve(addon, "manifest.json"), "utf8");
  return { manifest, executable, addon };
}
