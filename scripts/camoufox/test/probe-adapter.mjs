import { Adapter } from "../adapter.mjs";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { resolve } from "node:path";
import { tmpdir } from "node:os";
const root = await mkdtemp(resolve(tmpdir(), "oxbot-adapter-"));
const adapter = await Adapter.start({
  bundle: resolve("target/camoufox-bundle"),
  profile: resolve(root, "profile"),
  headless: true,
});
try {
  await mkdir(resolve(root, "work/downloads"), { recursive: true });
  await adapter.admin("downloads", { dir: resolve(root, "work/downloads") });
  const page = adapter.page();
  await page.setContent(
    '<label>Search <input type="search"></label><button>Add milk</button><p>Visible groceries</p>',
  );
  console.log(await adapter.snapshot());
} finally {
  await adapter.runtime.close();
  await rm(root, { recursive: true, force: true });
}
