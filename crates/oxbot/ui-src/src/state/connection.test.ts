import { describe, expect, it } from "vitest";
import type { AppSettings, ServerMsg } from "../net/protocol";
import { connectionReducer, initialConnection } from "./connection";

const server = (msg: ServerMsg) => ({ kind: "server", msg }) as const;

const settings: AppSettings = {
  version: "0.1.0",
  data_dir: "/tmp/oxbot",
  launch_at_login: null,
  you: "Jay",
  home_computer: true,
  usage_alerts: true,
  dictation: false,
  owner: true,
  dev_tools: false,
  owner_name: "Jay",
  openrouter_ready: false,
  codex_ready: false,
  keychain_waiting: false,
  keep_awake: null,
};

describe("connection reducer", () => {
  it("starts connecting, with no server yet", () => {
    expect(initialConnection.status).toBe("connecting");
    expect(initialConnection.serverName).toBe("");
  });

  it("takes the server's name, version and start time from hello", () => {
    const next = connectionReducer(
      initialConnection,
      server({
        type: "hello",
        protocol_version: 1,
        server_name: "jays-mac",
        server_version: "0.1.0",
        started_ms: 42,
      }),
    );
    expect(next.serverName).toBe("jays-mac");
    expect(next.serverVersion).toBe("0.1.0");
    expect(next.startedMs).toBe(42);
  });

  it("keeps the settings without the message tag", () => {
    const next = connectionReducer(initialConnection, server({ type: "settings", ...settings }));
    expect(next.settings).toEqual(settings);
    expect(next.settings).not.toHaveProperty("type");
  });

  it("remembers a connection opened once, even after it closes", () => {
    const open = connectionReducer(initialConnection, { kind: "status", status: "open" });
    expect(open.everConnected).toBe(true);
    const closed = connectionReducer(open, { kind: "status", status: "closed" });
    expect(closed.status).toBe("closed");
    expect(closed.everConnected).toBe(true);
  });

  it("returns the same object when nothing changed", () => {
    expect(connectionReducer(initialConnection, { kind: "status", status: "connecting" })).toBe(
      initialConnection,
    );
    expect(
      connectionReducer(initialConnection, server({ type: "bots", bots: [] })),
    ).toBe(initialConnection);
  });
});
