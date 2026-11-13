// Private, parent-owned stdio RPC. Models only reach the separate MCP facade.
import { createInterface } from "node:readline";
import { resolve } from "node:path";
import { Adapter } from "./adapter.mjs";
const options = {};
for (let i = 2; i < process.argv.length; i += 2) {
  const name = {
    "--bundle": "bundle",
    "--profile": "profile",
    "--headless": "headless",
    "--proxy": "proxy",
  }[process.argv[i]];
  if (!name || !process.argv[i + 1])
    throw new Error("Invalid browser helper arguments");
  options[name] =
    name === "headless"
      ? process.argv[i + 1] === "true"
      : name === "proxy"
        ? process.argv[i + 1]
        : resolve(process.argv[i + 1]);
}
if (!options.bundle || !options.profile)
  throw new Error("Missing browser bundle/profile");
const emit = (value) => process.stdout.write(JSON.stringify(value) + "\n");
const adapter = await Adapter.start(options, emit);
let stopping = false,
  queue = Promise.resolve();
const stop = async () => {
  if (stopping) return;
  stopping = true;
  adapter.closing = true;
  await adapter.runtime.close().catch(() => {});
  emit({ event: "closed" });
  process.stdin.destroy();
};
adapter.context.on("close", () => {
  if (!adapter.closing) void stop();
});
adapter.context.on("page", (page) =>
  page.on("close", () => {
    if (!adapter.closing && adapter.pages.size === 0) void stop();
  }),
);
for (const page of adapter.context.pages())
  page.on("close", () => {
    if (!adapter.closing && adapter.pages.size === 0) void stop();
  });
process.on("SIGTERM", () => {
  void stop();
});
process.on("SIGINT", () => {
  void stop();
});
emit({ event: "ready" });
const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
try {
  for await (const line of lines) {
    if (line.length > 2_000_000)
      throw new Error("Browser command is too large");
    const message = JSON.parse(line);
    if (message.method === "shutdown") {
      await stop();
      break;
    }
    queue = queue.then(async () => {
      try {
        if (stopping) throw new Error("The browser is stopping");
        // oxbot stops waiting after 60 s; past that, later calls must not
        // queue behind this one. Playwright's own timeouts end it later.
        let timer;
        const result = await Promise.race([
          adapter.admin(message.method, message.params ?? {}),
          new Promise((_, reject) => {
            timer = setTimeout(
              () => reject(new Error("The browser took too long; try again")),
              55000,
            );
          }),
        ]).finally(() => clearTimeout(timer));
        if (message.id !== undefined) emit({ id: message.id, result });
      } catch (error) {
        if (message.id !== undefined)
          emit({ id: message.id, error: String(error.message).slice(0, 1000) });
      }
      if (message.method === "shutdown") await stop();
    });
  }
} catch (error) {
  if (!stopping) process.stderr.write(String(error.message) + "\n");
} finally {
  await queue;
  await stop();
}
