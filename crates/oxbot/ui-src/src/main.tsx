// The browser entry: reads the page's address and key, connects, feeds the
// store and draws App. Browser-only code lives here and in web/; net/ and
// state/ stay platform-neutral.
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { type App as AppApi, AppContext } from "./app/context";
import { connect, socketUrl } from "./net/socket";
import { createStore, local } from "./state/store";
import { sessionStore, takeKey } from "./web/address";
import { LOGO } from "./components/brand";
import { expose, exposeFn } from "./web/testBridge";
import "./theme/tokens.css";
import "./theme/base.css";
import "./components/ui/index";

const key = takeKey(location, sessionStore(), (url) => history.replaceState(null, "", url));
// Test mode only: ?as=Name connects as that person (the server ignores it otherwise).
const as = new URLSearchParams(location.search).get("as");
const url = socketUrl({ host: location.host, secure: location.protocol === "https:", key, as });
const device = window.oxbotDevice;

const store = createStore();
const socket = connect({
  open: () => (device ? device.socket() : new WebSocket(url)),
  phone: Boolean(device),
  // A phone in the background can't reach the network anyway: wait until
  // it's opened again (visibilitychange below) instead of draining battery.
  holdRetries: () => Boolean(device) && document.hidden,
});
window.addEventListener("online", () => socket.reconnectNow());
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) socket.reconnectNow();
});
socket.onStatus((status) => {
  store.dispatch({ kind: "status", status });
  if (status === "open") {
    socket.send({ type: "list_bots" });
    socket.send({ type: "list_engines" });
  }
});
socket.onMessage((msg) => {
  // The server restarted (e.g. dev mode rebuilt it): reload to get its page.
  const known = store.getState().connection.startedMs;
  if (msg.type === "hello" && known !== null && msg.started_ms !== known) {
    location.reload();
    return;
  }
  store.dispatch({ kind: "server", msg });
});

const app: AppApi = {
  store,
  send: (msg) => socket.send(msg),
  dispatch: (action) => store.dispatch(action),
  toast: (text, error = false) => store.dispatch(local("ui/toast", { text, error })),
  hasKey: key !== "" || as !== null,
  key,
  phone: device ? { ...(device.homeComputer ? { homeComputer: device.homeComputer } : {}), ...(device.person ? { person: device.person } : {}) } : null,
};
exposeFn("send", app.send as never);
exposeFn("toast", app.toast as never);

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <AppContext.Provider value={app}>
        <App />
      </AppContext.Provider>
    </StrictMode>,
  );
}

// The tab icon.
const icon = document.querySelector<HTMLLinkElement>("link[rel=icon]") ?? document.head.appendChild(Object.assign(document.createElement("link"), { rel: "icon" }));
icon.href = `data:image/svg+xml,${encodeURIComponent(LOGO)}`;

// Tests read bots.get(id).info, as on the hand-written page.
expose("bots", () => {
  const byId = store.getState().bots.byId;
  return new Map(Object.entries(byId).map(([id, info]) => [id, { info }]));
});
