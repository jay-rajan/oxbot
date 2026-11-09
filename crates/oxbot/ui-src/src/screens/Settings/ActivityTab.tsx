// Settings → Activity: what left the home computer, newest first, and
// oxbot's own log folded away below. The server decides what each person
// may see (the owner: everything).
import { Fragment, useEffect, useRef } from "react";
import { type App, useApp, useSel } from "../../app/context";
import { Button, cx, Hint, List, ListRow, Note, Section } from "../../components/ui";
import type { NetKind, NetRow } from "../../net/protocol";
import { local } from "../../state/store";
import { NET_KINDS, bytes, money, plural, swappedText } from "./format";

const CHIPS: [NetKind | "", string][] = [
  ["", "All"],
  ["ai", "AI"],
  ["website", "Websites"],
  ["device", "Devices"],
  ["oxbot", "oxbot"],
];

/** Asks for the newest page of the chosen kind, or the page before `before`. */
export function askNet(app: App, before?: number): void {
  const kind = app.store.getState().activity.net.kind;
  app.send({ type: "list_net_log", kind: kind || null, before_ms: before ?? null, limit: 100 });
}

export function ActivityTab({ hidden }: { hidden: boolean }) {
  const app = useApp();
  const { dispatch, store } = app;
  const net = useSel((s) => s.activity.net);
  const t = net.totals;

  // Fresh while it's on screen; older pages stay as they were.
  useEffect(() => {
    if (hidden) return;
    const timer = setInterval(() => {
      if (document.visibilityState !== "visible") return;
      const n = store.getState().activity.net;
      if (n.open === null && n.rows.length <= 100) askNet(app);
    }, 15000);
    return () => clearInterval(timer);
  }, [hidden, store, app]);

  let day = "";
  return (
    <div id="setActivity" className="settings-pane" role="tabpanel" hidden={hidden}>
      <Section>
        <div className="net-chips" role="radiogroup" aria-label="Show">
          {CHIPS.map(([kind, label]) => (
            <button
              key={kind}
              type="button"
              role="radio"
              className="net-chip"
              data-net={kind}
              aria-checked={net.kind === kind}
              onClick={() => {
                dispatch(local("activity/netKind", { netKind: kind }));
                askNet(app);
              }}
            >
              {label}
            </button>
          ))}
        </div>
        <Hint id="netTotals">
          {t
            ? `Today: ${plural(t.today_rows, "exchange")} · ${bytes(t.today_up)} sent · ${bytes(t.today_down)} received · ${money(t.today_cost_usd || 0)} of AI`
            : "Loading…"}
        </Hint>
        <List className="net-rows" id="netRows">
          {t && !net.rows.length ? (
            <li>
              <Note>Nothing yet.</Note>
            </li>
          ) : null}
          {net.rows.map((r) => {
            const d = new Date(r.ts_ms).toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" });
            const head = d !== day;
            day = d;
            return (
              <Fragment key={r.id}>
                {head ? <li className="net-day">{d}</li> : null}
                <Row r={r} open={net.open === r.id} onToggle={() => dispatch(local("activity/netToggle", { id: r.id }))} />
              </Fragment>
            );
          })}
        </List>
        <div className="btn-row" hidden={!net.more}>
          <Button
            size="sm"
            id="netMore"
            hidden={!net.more}
            onClick={() => {
              const last = net.rows[net.rows.length - 1];
              if (!last) return;
              dispatch(local("activity/netOlder"));
              askNet(app, last.ts_ms);
            }}
          >
            Show older
          </Button>
        </div>
        <Note>What left the home computer, kept 30 days. Sizes and counts only, never what was sent.</Note>
      </Section>
      <details className="oxlog">
        <summary>oxbot's own log</summary>
        <LogList />
      </details>
    </div>
  );
}

function Row({ r, open, onToggle }: { r: NetRow; open: boolean; onToggle: () => void }) {
  const failed = r.outcome && r.outcome !== "ok";
  const lines = [`${NET_KINDS[r.kind] || r.kind} · ${r.host}`, `took ${(r.ms / 1000).toFixed(1)}s · ${r.outcome || "ok"}`];
  if (r.person) lines.push(`for ${r.person}`);
  if (r.other_hosts) lines.push(`the page also pulled from ${plural(r.other_hosts, "other site")}`);
  return (
    <ListRow
      className={cx("net-row", !!failed && "failed")}
      tabIndex={0}
      role="button"
      aria-expanded={open}
      onClick={onToggle}
      onKeyDown={(ev) => {
        if (ev.key === "Enter" || ev.key === " ") {
          ev.preventDefault();
          onToggle();
        }
      }}
      lead={<span className="t">{new Date(r.ts_ms).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}</span>}
      title={
        <span className="what">
          <span>{`${r.who} → ${r.kind === "ai" ? "AI" : r.host}`}</span>
          <small>{[r.label, swappedText(r.swapped), failed ? r.outcome : ""].filter(Boolean).join(" · ")}</small>
        </span>
      }
      trail={
        <span className="size">
          {`${bytes(r.bytes_up)} ↑ ${bytes(r.bytes_down)} ↓`}
          {r.cost_usd != null ? <small>{money(r.cost_usd)}</small> : null}
        </span>
      }
    >
      {open ? (
        <div className="more">
          {lines.map((l) => (
            <span key={l}>{l}</span>
          ))}
        </div>
      ) : null}
    </ListRow>
  );
}

/** oxbot's own log lines (the home computer's), newest at the bottom. */
function LogList() {
  const logs = useSel((s) => s.home.logs);
  const ref = useRef<HTMLOListElement>(null);
  useEffect(() => {
    const list = ref.current;
    if (list) list.scrollTop = list.scrollHeight;
  }, [logs]);
  return (
    <ol className="loglist" id="logList" ref={ref}>
      {logs.map((l, i) => (
        <li key={`${l.ts_ms}-${i}`} className={l.level} title={l.target}>
          <span>{new Date(l.ts_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}</span>
          <span>{l.level}</span>
          <span>{l.message}</span>
        </li>
      ))}
    </ol>
  );
}
