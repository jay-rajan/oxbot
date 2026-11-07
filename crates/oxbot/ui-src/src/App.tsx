import { useMemo } from "react";
import { botList } from "./state/bots";
import { type Store, useStore } from "./state/store";
import "./App.css";

/** Step one of the React page: proves the pipeline (socket, store, types)
 *  by showing the server and its bots. Screens come over one by one. */
export function App({ store, hasKey }: { store: Store; hasKey: boolean }) {
  const connection = useStore(store, (s) => s.connection);
  const botsState = useStore(store, (s) => s.bots);
  const bots = useMemo(() => botList(botsState), [botsState]);

  let status: string;
  if (connection.status === "open") status = connection.serverName || "Connected";
  else if (!hasKey && !connection.everConnected && connection.status === "closed")
    status = "This window isn't oxbot's own. Open oxbot from its app, or with the link `oxbot serve` printed.";
  else if (connection.everConnected) status = "Lost the connection to oxbot. Reconnecting…";
  else status = "Connecting…";

  return (
    <main className="next">
      <header>
        <h1>oxbot</h1>
        <p id="server" className="muted">
          {status}
        </p>
      </header>
      <section aria-labelledby="botsTitle">
        <h2 id="botsTitle">Bots</h2>
        <ul id="bots">
          {bots.map((b) => (
            <li key={b.id} data-id={b.id}>
              {b.name}
            </li>
          ))}
        </ul>
      </section>
    </main>
  );
}
