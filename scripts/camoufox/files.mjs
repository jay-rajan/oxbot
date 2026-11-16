import { lstat, realpath } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";
const outside = (rel) =>
  !rel || rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel);
export async function workspaceFile(root, name, { existing = false } = {}) {
  const original = resolve(root),
    base = await realpath(original);
  const requested = isAbsolute(name) ? resolve(name) : resolve(original, name);
  // Accept the known workspace's spelling and its canonical spelling (macOS
  // /var -> /private/var), without following a model-supplied link within it.
  let rel = relative(original, requested);
  if (outside(rel)) rel = relative(base, requested);
  if (outside(rel))
    throw new Error("Files must stay inside the active bot workspace");
  const file = resolve(base, rel);
  let current = base;
  for (const part of rel.split(sep)) {
    current = resolve(current, part);
    try {
      if ((await lstat(current)).isSymbolicLink())
        throw new Error("Files through symbolic links are refused");
    } catch (error) {
      if (error.code !== "ENOENT" || existing) throw error;
    }
  }
  return file;
}
