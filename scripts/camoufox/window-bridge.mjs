import { createServer } from "node:http";
import { randomBytes, randomUUID } from "node:crypto";

// Internal lifecycle channel, never exposed as a model tool. It carries only
// window states; the high-entropy path is private to the temporary extension.
export async function windowBridge({ timeout = 10000 } = {}) {
  const token = randomBytes(32).toString("hex");
  let poll,
    pending,
    ended = false;
  const server = createServer(async (req, res) => {
    res.setHeader("Cache-Control", "no-store");
    const origin = req.headers.origin;
    if (
      req.headers.host !== `127.0.0.1:${server.address().port}` ||
      (origin && !/^moz-extension:\/\/[0-9a-f-]{36}$/.test(origin))
    ) {
      res.writeHead(403).end();
      return;
    }
    if (req.method === "GET" && req.url === `/${token}/next`) {
      if (ended) {
        res.writeHead(410).end();
        return;
      }
      if (poll) {
        res.writeHead(409).end();
        return;
      }
      poll = res;
      const timer = setTimeout(() => {
        if (poll === res) poll = undefined;
        res.writeHead(204).end();
      }, 30000);
      res.on("close", () => {
        clearTimeout(timer);
        if (poll === res) poll = undefined;
      });
      dispatch();
    } else if (req.method === "POST" && req.url === `/${token}/result`) {
      let bytes = 0,
        parts = [];
      try {
        for await (const part of req) {
          bytes += part.length;
          if (bytes > 4096) throw new Error("Oversized result");
          parts.push(part);
        }
        const result = JSON.parse(Buffer.concat(parts).toString());
        if (!pending || result.id !== pending.id) {
          res.writeHead(409).end();
          return;
        }
        const { finish } = pending;
        pending = undefined;
        if (result.error) finish(new Error("Browser window control failed"));
        else if (
          !Array.isArray(result.states) ||
          !result.states.length ||
          result.states.some(
            (s) =>
              ![
                "normal",
                "minimized",
                "maximized",
                "fullscreen",
                "docked",
              ].includes(s),
          )
        )
          finish(new Error("Invalid browser window state"));
        else finish(null, result.states);
        res.writeHead(204).end();
      } catch {
        res.writeHead(400).end();
      }
    } else {
      res.writeHead(404).end();
    }
  });
  server.requestTimeout = 10000;
  server.headersTimeout = 10000;
  function dispatch() {
    if (!poll || !pending || pending.sent) return;
    const response = poll;
    poll = undefined;
    pending.sent = true;
    response.setHeader("Content-Type", "application/json");
    response.end(JSON.stringify({ id: pending.id, state: pending.state }));
  }
  await new Promise((done, fail) => {
    server.once("error", fail);
    server.listen(0, "127.0.0.1", done);
  });
  return {
    endpoint: `http://127.0.0.1:${server.address().port}/${token}`,
    setState(state) {
      if (ended || pending)
        return Promise.reject(
          new Error("Window bridge is unavailable or busy"),
        );
      if (!["minimized", "normal", "maximized", "status"].includes(state))
        return Promise.reject(new Error("Invalid window command"));
      return new Promise((done, fail) => {
        const id = randomUUID();
        const timer = setTimeout(() => {
          if (pending?.id === id) pending = undefined;
          fail(new Error("Browser window control did not respond"));
        }, timeout);
        pending = {
          id,
          state,
          sent: false,
          finish: (error, states) => {
            clearTimeout(timer);
            error ? fail(error) : done(states);
          },
        };
        dispatch();
      });
    },
    async close() {
      ended = true;
      if (pending) {
        pending.finish(new Error("Window bridge closed"));
        pending = undefined;
      }
      if (poll) {
        poll.writeHead(410).end();
        poll = undefined;
      }
      server.closeAllConnections();
      await new Promise((done) => server.close(done));
    },
  };
}
