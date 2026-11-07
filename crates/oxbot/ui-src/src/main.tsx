// The browser entry: reads the page's address and key, connects, feeds the
// store and draws App. Browser-only code lives here and in web/; net/ and
// state/ stay platform-neutral.
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { connect, socketUrl } from "./net/socket";
import { createStore } from "./state/store";
import { sessionStore, takeKey } from "./web/address";
import "./tokens.css";

const key = takeKey(location, sessionStore(), (url) => history.replaceState(null, "", url));
// Test mode only: /next/?as=Name connects as that person (the server ignores it otherwise).
const as = new URLSearchParams(location.search).get("as");
const url = socketUrl({ host: location.host, secure: location.protocol === "https:", key, as });
const gate = window.oxbotGate;

const store = createStore();
const socket = connect({
  open: () => (gate ? gate.socket() : new WebSocket(url)),
  phone: Boolean(gate),
  // A phone in the background can't reach the network anyway: wait until
  // it's opened again (visibilitychange below) instead of draining battery.
  holdRetries: () => Boolean(gate) && document.hidden,
});
window.addEventListener("online", () => socket.reconnectNow());
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) socket.reconnectNow();
});
socket.onStatus((status) => store.dispatch({ kind: "status", status }));
socket.onMessage((msg) => {
  // The server restarted (e.g. dev mode rebuilt it): reload to get its page.
  const known = store.getState().connection.startedMs;
  if (msg.type === "hello" && known !== null && msg.started_ms !== known) {
    location.reload();
    return;
  }
  store.dispatch({ kind: "server", msg });
});

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <App store={store} hasKey={key !== "" || as !== null} />
    </StrictMode>,
  );
}
