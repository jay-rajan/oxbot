// The right-click menu: a bot's (the same rules as its ··· settings) or a
// section's. Right-click, a long press on a phone, or the menu key opens it;
// a press elsewhere, Escape, Tab, scrolling the list or resizing closes it.
import { type KeyboardEvent, type MouseEvent, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useApp, useSel } from "../../app/context";
import type { BotInfo } from "../../net/protocol";
import { local } from "../../state/store";
import type { NameAsk as AskName } from "../Welcome/NameDialog";

export type MenuFor = { bot: string; section?: undefined } | { section: string; bot?: undefined };
export interface MenuAt {
  for: MenuFor;
  x: number;
  y: number;
}

interface Item {
  label?: string;
  rule?: boolean;
  confirm?: string;
  danger?: boolean;
  disabled?: boolean;
  title?: string;
  sub?: boolean;
  expanded?: boolean;
  indent?: boolean;
  checked?: boolean;
  act?: () => void;
}

/** What the bot settings say about who sees a bot, when it's made private. */
export function privacyHintPrivate(b: BotInfo): string {
  const maker = b.created_by || "its maker";
  return `Only ${maker} and the owner see it, its chat and its reports. ${maker}'s own Assistant can give it tasks; it asks ${maker} first if it has read web content or other bots' replies. It uses the websites ${maker} is signed in to, never the home's. Personal details are swapped for stand-ins before anything is sent; details only described in words, and names oxbot was never told, aren't.`;
}

export function menuKey(ev: KeyboardEvent): boolean {
  return ev.key === "ContextMenu" || (ev.key === "F10" && ev.shiftKey);
}

/** Focuses the row or section header a menu was for (after it closes). */
export function refocus(f: MenuFor): void {
  const nav = document.getElementById("bots");
  if (!nav) return;
  const el = f.bot
    ? [...nav.querySelectorAll<HTMLElement>(".bot-item")].find((e) => e.dataset.bot === f.bot)
    : [...nav.querySelectorAll<HTMLElement>(".group")].find((g) => g.dataset.key === `s:${f.section}`)?.querySelector<HTMLElement>(".group-head");
  el?.focus();
}

export function BotMenu({
  at,
  close,
  askName,
}: {
  at: MenuAt | null;
  close: (refocusAfter?: boolean) => void;
  askName: (a: AskName) => void;
}) {
  const { send, dispatch, toast } = useApp();
  const byId = useSel((s) => s.bots.byId);
  const sections = useSel((s) => s.home.sections);
  const placed = useSel((s) => s.home.placed);
  const busyNow = useSel((s) => s.sidebar.busy);
  const settings = useSel((s) => s.connection.settings);
  const [openSub, setOpenSub] = useState(false);
  const [armed, setArmed] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const menuFor = at?.for ?? null;
  const target = menuFor?.bot ?? (menuFor ? `s:${menuFor.section}` : null);

  // A new menu starts with the section list folded and nothing armed.
  useEffect(() => {
    setOpenSub(false);
    setArmed(null);
  }, [at]);
  // Two-step: the first press relabels the item, the second does it; after 5 s it goes back.
  useEffect(() => {
    if (!armed) return;
    const t = setTimeout(() => setArmed(null), 5000);
    return () => clearTimeout(t);
  }, [armed]);

  // Gone (deleted here or on another device): the menu closes.
  const gone = !!menuFor && (menuFor.bot ? !byId[menuFor.bot] : !sections.some((x) => x.id === menuFor.section));
  useEffect(() => {
    if (gone) close();
  }, [gone, close]);

  const open = !!at && !gone;
  // Escape closes it first (before the phone drawer or a dialog); a press
  // anywhere else, or the window changing, closes it.
  useEffect(() => {
    if (!open) return;
    const key = (ev: globalThis.KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        close(true);
      }
    };
    const down = (ev: PointerEvent) => {
      if (!ref.current?.contains(ev.target as Node)) close();
    };
    const resize = () => close();
    document.addEventListener("keydown", key, true);
    document.addEventListener("pointerdown", down, true);
    window.addEventListener("resize", resize);
    return () => {
      document.removeEventListener("keydown", key, true);
      document.removeEventListener("pointerdown", down, true);
      window.removeEventListener("resize", resize);
    };
  }, [open, close]);

  const items: Item[] = [];
  if (open && menuFor?.bot) {
    const id = menuFor.bot;
    const b = byId[id];
    if (b) {
      const name = b.name || id;
      if (b.can_manage) {
        items.push({
          label: "Rename…",
          act: () => {
            close();
            askName({
              title: `Rename ${name}`,
              value: name,
              max: 60,
              done: (n) => {
                if (n && n !== b.name) send({ type: "rename_bot", bot: id, name: n });
              },
            });
          },
        });
        const busy = !!(busyNow[id] ?? b.busy);
        const wait = "Wait until it's done working";
        if (b.private) {
          items.push({
            label: "Make shared",
            confirm: "Yes, share with everyone",
            disabled: busy,
            title: busy ? wait : "Everyone sees and uses it, and its chat",
            act: () => {
              close(true);
              send({ type: "set_private", bot: id, private: false });
            },
          });
        } else {
          items.push({
            label: "Make private",
            disabled: busy,
            title: busy ? wait : "",
            act: () => {
              close(true);
              send({ type: "set_private", bot: id, private: true });
              toast(privacyHintPrivate(b));
            },
          });
        }
      }
      items.push({ label: "Move to section", sub: true, expanded: openSub, act: () => setOpenSub((o) => !o) });
      if (openSub) {
        for (const sec of sections)
          items.push({
            label: sec.name,
            indent: true,
            checked: placed[id] === sec.id,
            act: () => {
              close(true);
              if (placed[id] !== sec.id) send({ type: "move_bot", bot: id, section: sec.id });
            },
          });
        items.push({
          label: "Not in a section",
          indent: true,
          checked: !sections.some((s) => s.id === placed[id]),
          act: () => {
            close(true);
            if (placed[id]) send({ type: "move_bot", bot: id, section: null });
          },
        });
        items.push({
          label: "New section…",
          indent: true,
          act: () => {
            close();
            askName({ title: "New section", value: "", max: 40, done: (n) => send({ type: "create_section", name: n, bot: id }) });
          },
        });
      }
      // Compare is a developer tool: only the owner, and only with developer tools on.
      if (settings?.owner && settings.dev_tools && !b.central && !b.locked)
        items.push({
          label: "Compare…",
          act: () => {
            close();
            dispatch(local("ui/open", { dialog: "compare", arg: id }));
          },
        });
      if (b.can_manage) {
        items.push({ rule: true });
        items.push({
          label: "Delete…",
          confirm: `Yes, delete ${name}`,
          danger: true,
          act: () => {
            close();
            send({ type: "delete_bot", bot: id });
          },
        });
      }
    }
  } else if (open && menuFor?.section) {
    const sec = sections.find((s) => s.id === menuFor.section);
    if (sec) {
      items.push({
        label: "Rename…",
        act: () => {
          close();
          askName({
            title: `Rename ${sec.name}`,
            value: sec.name,
            max: 40,
            done: (n) => {
              if (n !== sec.name) send({ type: "rename_section", id: sec.id, name: n });
            },
          });
        },
      });
      items.push({
        label: "Delete",
        confirm: `Yes, delete ${sec.name} (its bots stay)`,
        danger: true,
        act: () => {
          close(true);
          send({ type: "delete_section", id: sec.id });
        },
      });
    }
  }
  const who = menuFor?.bot ? byId[menuFor.bot]?.name : sections.find((s) => s.id === menuFor?.section)?.name;

  // Kept inside the window; the first item takes focus when it opens.
  const opened = useRef<string | null>(null);
  useLayoutEffect(() => {
    const menu = ref.current;
    if (!menu || !open || !at) {
      opened.current = null;
      return;
    }
    const r = menu.getBoundingClientRect();
    menu.style.left = `${Math.max(8, Math.min(at.x, innerWidth - r.width - 8))}px`;
    menu.style.top = `${Math.max(8, Math.min(at.y, innerHeight - r.height - 8))}px`;
    if (opened.current !== target) {
      opened.current = target;
      menu.querySelector("button")?.focus();
    }
  });

  const keyDown = (ev: KeyboardEvent<HTMLDivElement>) => {
    const list = [...(ref.current?.querySelectorAll("button") ?? [])];
    const i = list.indexOf(document.activeElement as HTMLButtonElement);
    if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
      ev.preventDefault();
      const n = list.length;
      const down = ev.key === "ArrowDown";
      if (n) list[i < 0 ? (down ? 0 : n - 1) : (i + (down ? 1 : n - 1)) % n]?.focus();
    } else if (ev.key === "Home" || ev.key === "End") {
      ev.preventDefault();
      (ev.key === "Home" ? list[0] : list[list.length - 1])?.focus();
    } else if (ev.key === "Tab") close(true);
  };

  return createPortal(
    <div className="pop-menu" id="botMenu" role="menu" hidden={!open} ref={ref} aria-label={open ? who || "Menu" : undefined} onKeyDown={keyDown}>
      {open
        ? items.map((it, n) => {
            if (it.rule) return <hr key={`rule${n}`} role="separator" />;
            const key = `${it.indent ? "sub:" : ""}${it.label}`;
            const isArmed = !!it.confirm && armed === key;
            const click = (ev: MouseEvent) => {
              ev.stopPropagation();
              if (it.disabled) return;
              if (it.confirm && !isArmed) {
                setArmed(key);
                return;
              }
              it.act?.();
            };
            const cls = [it.indent ? "sub" : "", it.danger || isArmed ? "danger" : ""].filter(Boolean).join(" ");
            return (
              <button
                key={key}
                type="button"
                role={it.checked !== undefined ? "menuitemradio" : "menuitem"}
                aria-checked={it.checked !== undefined ? String(it.checked) as "true" | "false" : undefined}
                aria-haspopup={it.sub ? "true" : undefined}
                aria-expanded={it.sub ? String(!!it.expanded) as "true" | "false" : undefined}
                className={cls || undefined}
                title={it.title || undefined}
                aria-disabled={it.disabled ? "true" : undefined}
                onClick={click}
              >
                {isArmed ? it.confirm : it.sub ? `${it.label} ${it.expanded ? "▾" : "▸"}` : it.label}
              </button>
            );
          })
        : null}
    </div>,
    document.body,
  );
}
