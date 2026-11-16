// Per-bot MCP facade. No admin methods, cookies, evaluation, or browser endpoint
// other than the Rust broker's restricted public-tool route are exposed.
import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";
import { resolve } from "node:path";
import { tools, names } from "./tool-specs.mjs";
const options = {};
for (let i = 2; i < process.argv.length; i += 2) {
  const key = { "--endpoint": "endpoint", "--workspace": "workspace" }[
    process.argv[i]
  ];
  if (!key || !process.argv[i + 1]) throw new Error("Invalid tool arguments");
  options[key] = process.argv[i + 1];
}
const endpoint = new URL(options.endpoint);
if (endpoint.protocol !== "http:" || endpoint.hostname !== "127.0.0.1")
  throw new Error("Browser tools require the private local broker");
const workspace = resolve(options.workspace);
const output = (value) => process.stdout.write(JSON.stringify(value) + "\n");
let queue = Promise.resolve();
const active = new Map();
const cancel = (record) => {
  record.controller.abort();
  if (record.started)
    void fetch(endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ operation: record.operation, cancel: true }),
      signal: AbortSignal.timeout(3000),
    }).catch(() => {});
};
for await (const line of createInterface({
  input: process.stdin,
  crlfDelay: Infinity,
})) {
  if (line.length > 2_000_000) throw new Error("Tool request is too large");
  const request = JSON.parse(line);
  if (request.method === "notifications/cancelled") {
    const record = active.get(JSON.stringify(request.params?.requestId));
    if (record) cancel(record);
    continue;
  }
  if (request.id === undefined) continue;
  const record = {
    operation: randomUUID(),
    controller: new AbortController(),
    started: false,
  };
  if (request.method === "tools/call")
    active.set(JSON.stringify(request.id), record);
  queue = queue.then(async () => {
    try {
      let result;
      if (request.method === "initialize")
        result = {
          protocolVersion: request.params?.protocolVersion ?? "2024-11-05",
          capabilities: { tools: {} },
          serverInfo: { name: "oxbot-browser", version: "1" },
        };
      else if (request.method === "ping") result = {};
      else if (request.method === "tools/list") result = { tools };
      else if (request.method === "tools/call") {
        if (record.controller.signal.aborted)
          throw new Error("Browser action cancelled before it ran");
        const name = request.params?.name;
        if (!names.has(name)) throw new Error("Unsupported browser tool");
        record.started = true;
        const response = await fetch(endpoint, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            name,
            args: request.params.arguments ?? {},
            workspace,
            operation: record.operation,
          }),
          signal: AbortSignal.any([
            AbortSignal.timeout(120000),
            record.controller.signal,
          ]),
        });
        const body = await response.json();
        record.finished = true;
        if (!response.ok || body.error)
          throw new Error(body.error ?? "The browser is unavailable");
        const content = [{ type: "text", text: body.text ?? "" }];
        if (body.image)
          content.push({
            type: "image",
            mimeType: "image/png",
            data: body.image,
          });
        result = { content, isError: false };
      } else {
        output({
          jsonrpc: "2.0",
          id: request.id,
          error: { code: -32601, message: "Unknown method" },
        });
        return;
      }
      output({ jsonrpc: "2.0", id: request.id, result });
    } catch (error) {
      if (record.started && !record.finished) cancel(record);
      output({
        jsonrpc: "2.0",
        id: request.id,
        result: {
          content: [
            { type: "text", text: String(error.message).slice(0, 1000) },
          ],
          isError: true,
        },
      });
    } finally {
      active.delete(JSON.stringify(request.id));
    }
  });
}
for (const record of active.values()) cancel(record);
await queue;
