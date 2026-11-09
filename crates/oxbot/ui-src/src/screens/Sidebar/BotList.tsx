// The bot list: Assistant first, then bots by section or by who made them,
// each group foldable; a bot can be dragged onto a section, or out of them.
import { type DragEvent, type KeyboardEvent, type MouseEvent, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { avatarSvg } from "../../components/brand";
import { Button, ChevronIcon, Note, Pill, vars } from "../../components/ui";
import { botList } from "../../state/bots";
import { waitingBots } from "../../state/cards";
import { type Group, fold, groupBots, isUnread } from "../../state/sidebar";
import { local } from "../../state/store";
import type { NameAsk as AskName } from "../Welcome/NameDialog";
import { type MenuFor, menuKey } from "./BotMenu";

const DRAG_TYPE = "application/x-oxbot-bot";

function Lock() {
  return (
    <svg className="lock" viewBox="0 0 12 12" aria-label="Private" role="img">
      <rect x="2" y="5.2" width="8" height="5.8" rx="1.3" fill="currentColor" />
      <path d="M3.9 5.4V3.9a2.1 2.1 0 0 1 4.2 0v1.5" fill="none" stroke="currentColor" strokeWidth="1.4" />
    </svg>
  );
}

export function BotList({
  openMenu,
  closeMenu,
  askName,
}: {
  openMenu: (f: MenuFor, x: number, y: number) => void;
  closeMenu: () => void;
  askName: (a: AskName) => void;
}) {
  const { send, dispatch } = useApp();
  const bots = useSel((s) => s.bots);
  const sections = useSel((s) => s.home.sections);
  const placed = useSel((s) => s.home.placed);
  const people = useSel((s) => s.home.people);
  const you = useSel((s) => s.connection.settings?.you);
  const side = useSel((s) => s.sidebar);
  const current = useSel((s) => s.ui.current);
  const cards = useSel((s) => s.cards);
  const waiting = waitingBots(cards);
  const navRef = useRef<HTMLElement>(null);
  // Mid-drag the list stays as it is, so the drop lands where it's shown.
  const dragging = useRef<string | null>(null);
  const frozen = useRef<Group[]>([]);
  const [, redraw] = useState(0);

  const searching = !!fold(side.query).trim();
  const groups = dragging.current
    ? frozen.current
    : groupBots(botList(bots), sections, placed, side.groupBy, side.query, you);
  frozen.current = groups;

  // Dragging a bot onto a section, or out of them.
  const nav = () => navRef.current as HTMLElement;
  const dropGroup = (ev: DragEvent): HTMLElement => {
    const t = ev.target as Element | null;
    const g = t?.closest ? (t.closest(".group") as HTMLElement | null) : null;
    if (g && g.dataset.section !== undefined) return g;
    return nav().querySelector<HTMLElement>('.group[data-key="none"]') || nav();
  };
  const clearDrop = () => {
    for (const g of nav().querySelectorAll(".group.drop")) g.classList.remove("drop");
  };
  const endDrag = () => {
    const was = dragging.current;
    dragging.current = null;
    clearDrop();
    if (was) redraw((n) => n + 1);
  };
  const onDragStart = (ev: DragEvent) => {
    const row = (ev.target as Element).closest?.(".bot-item") as HTMLButtonElement | null;
    if (!row || !row.draggable) return;
    const id = row.dataset.bot ?? null;
    dragging.current = id;
    if (id) ev.dataTransfer.setData(DRAG_TYPE, id);
    ev.dataTransfer.effectAllowed = "move";
  };
  const onDragOver = (ev: DragEvent) => {
    if (!dragging.current) return;
    ev.preventDefault();
    ev.dataTransfer.dropEffect = "move";
    const g = dropGroup(ev);
    if (!g.classList.contains("drop")) {
      clearDrop();
      if (g.classList.contains("group")) g.classList.add("drop");
    }
  };
  const onDrop = (ev: DragEvent) => {
    const id = ev.dataTransfer?.getData(DRAG_TYPE) || dragging.current;
    if (!id || !bots.byId[id]) {
      endDrag();
      return;
    }
    ev.preventDefault();
    const g = dropGroup(ev);
    const section = g.dataset?.section || null;
    if ((placed[id] || null) !== section) send({ type: "move_bot", bot: id, section });
    endDrag();
  };

  const select = (id: string) => dispatch(local("ui/select", { bot: id }));
  const color = (name: string) => people.find((p) => p.name === name)?.color;

  const row = (id: string) => {
    const b = bots.byId[id];
    if (!b) return null;
    const busy = side.busy[id] ?? b.busy;
    const title = busy ? "Working…" : b.private ? (b.sharing === "people" ? "Shared with picked people" : "Private") : undefined;
    // Under the name: Assistant's job, or who the bot belongs to.
    const tag = b.central ? "leads your bots" : b.created_by ? `by ${b.created_by}` : "";
    return (
      <button
        key={id}
        className="bot-item"
        type="button"
        data-bot={id}
        aria-current={String(id === current) as "true" | "false"}
        title={title}
        draggable={side.groupBy === "sections" && !b.central ? true : undefined}
        onClick={() => select(id)}
        onContextMenu={(ev: MouseEvent) => {
          ev.preventDefault();
          openMenu({ bot: id }, ev.clientX, ev.clientY);
        }}
        onKeyDown={(ev: KeyboardEvent<HTMLButtonElement>) => {
          if (!menuKey(ev)) return;
          ev.preventDefault();
          const r = ev.currentTarget.getBoundingClientRect();
          openMenu({ bot: id }, r.left + 24, r.bottom);
        }}
      >
        {/* biome-ignore lint/security/noDangerouslySetInnerHtml: oxbot's own SVG */}
        <span className={`avatar${busy ? " working" : ""}`} dangerouslySetInnerHTML={{ __html: avatarSvg(id, b.central) }} />
        <span className="meta">
          <span className="name">
            <span>{b.name || id}</span>
            {b.private ? <Lock /> : null}
          </span>
          {tag ? <span className="central">{tag}</span> : null}
        </span>
        {waiting.has(id) || isUnread(side, id) ? (
          <span className="marks">
            {waiting.has(id) ? <Pill tone="warn" className="needs-you">needs you</Pill> : null}
            {isUnread(side, id) ? <span className="unread" role="img" aria-label="New messages" title="New messages" /> : null}
          </span>
        ) : null}
      </button>
    );
  };

  return (
    <nav
      className="bots"
      id="bots"
      aria-label="Bots"
      ref={navRef}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDrop={onDrop}
      onDragEnd={endDrag}
      onScroll={() => closeMenu()}
    >
      {groups.map((g, n) => {
        const open = searching || !side.collapsed.includes(g.key);
        const titleId = `groupTitle${n}`;
        const section = g.section;
        return (
          <div
            key={g.key}
            className="group"
            data-key={g.key}
            data-section={section !== undefined ? section || "" : undefined}
            role={g.title ? "group" : undefined}
            aria-labelledby={g.title ? titleId : undefined}
          >
            {g.title ? (
              <button
                type="button"
                className="group-head"
                aria-expanded={String(open) as "true" | "false"}
                onClick={() => {
                  if (!searching) dispatch(local("sidebar/toggle", { key: g.key }));
                }}
                onContextMenu={
                  section
                    ? (ev: MouseEvent) => {
                        ev.preventDefault();
                        openMenu({ section }, ev.clientX, ev.clientY);
                      }
                    : undefined
                }
                onKeyDown={
                  section
                    ? (ev: KeyboardEvent<HTMLButtonElement>) => {
                        if (!menuKey(ev)) return;
                        ev.preventDefault();
                        const r = ev.currentTarget.getBoundingClientRect();
                        openMenu({ section }, r.left + 24, r.bottom);
                      }
                    : undefined
                }
              >
                <span className="chev" aria-hidden="true">
                  <ChevronIcon />
                </span>
                {g.person !== undefined ? <i className="maker-dot person-dot" style={vars({ person: color(g.person) })} /> : null}
                <span className="title" id={titleId}>
                  {g.title}
                </span>
                <span className="count" title={g.ids.length === 1 ? "1 bot" : `${g.ids.length} bots`}>
                  {String(g.ids.length)}
                </span>
              </button>
            ) : null}
            {!g.title || open ? g.ids.map(row) : null}
          </div>
        );
      })}
      {searching && groups.length === 0 ? <Note>{`No bot called “${side.query.trim()}”.`}</Note> : null}
      {side.groupBy === "sections" && !searching ? (
        <Button
          kind="quiet"
          size="sm"
          className="new-section"
          onClick={() => askName({ title: "New section", value: "", max: 40, done: (name) => send({ type: "create_section", name, bot: null }) })}
        >
          + New section
        </Button>
      ) : null}
    </nav>
  );
}
