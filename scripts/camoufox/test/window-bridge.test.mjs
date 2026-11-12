import test from "node:test";
import assert from "node:assert/strict";
import { windowBridge } from "../window-bridge.mjs";

test("private window channel rejects other paths/origins and only accepts a matching result", async (t) => {
  const bridge = await windowBridge({ timeout: 3000 });
  t.after(() => bridge.close());
  const url = new URL(bridge.endpoint);
  assert.equal((await fetch(`${url.origin}/guess/next`)).status, 404);
  assert.equal(
    (
      await fetch(`${bridge.endpoint}/next`, {
        headers: { origin: "https://example.com" },
      })
    ).status,
    403,
  );
  const waiting = fetch(`${bridge.endpoint}/next`);
  const done = bridge.setState("minimized");
  const response = await waiting;
  assert.equal(response.headers.get("access-control-allow-origin"), null);
  const { id, state } = await response.json();
  assert.equal(state, "minimized");
  assert.equal(
    (
      await fetch(`${bridge.endpoint}/result`, {
        method: "POST",
        body: JSON.stringify({ id: "other", states: ["normal"] }),
      })
    ).status,
    409,
  );
  await assert.rejects(bridge.setState("normal"), /busy/);
  assert.equal(
    (
      await fetch(`${bridge.endpoint}/result`, {
        method: "POST",
        body: JSON.stringify({ id, states: ["minimized"] }),
      })
    ).status,
    204,
  );
  assert.deepEqual(await done, ["minimized"]);
});

test("absence of the window extension refuses startup instead of silently proceeding", async (t) => {
  const bridge = await windowBridge({ timeout: 30 });
  t.after(() => bridge.close());
  await assert.rejects(bridge.setState("minimized"), /did not respond/);
});

test("shutdown rejects waiting commands and closes pending long polls", async () => {
  const bridge = await windowBridge();
  const result = assert.rejects(bridge.setState("minimized"), /closed/);
  await bridge.close();
  await result;
  await assert.rejects(bridge.setState("normal"), /unavailable/);
});
