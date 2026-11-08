// What the chat does with the server: following a bot, paging back,
// opening a report, uploading files. Browser-side (it uploads with XHR).
import type { App } from "../../app/context";
import type { BotInfo } from "../../net/protocol";
import { type BotChat, type ChatState, PAGE, reportKey, uploading } from "../../state/chat";
import { local } from "../../state/store";
import { sessionStore, takeKey } from "../../web/address";

/** Whether it's someone else's private bot (listed, but it won't open). */
export const isLocked = (info: BotInfo | undefined) => !!info?.locked;

/** Starts following a bot: its latest page if we have nothing yet, else
 *  everything after what we have (a reconnect catches up this way). */
export function follow(app: App, id: string, b: BotChat | undefined) {
  if (b && b.lastSeq > 0) app.send({ type: "subscribe", bot: id, since: b.lastSeq });
  else app.send({ type: "open_bot", bot: id, limit: PAGE });
}

/** Asks for the page before the oldest one shown, once the chat is
 *  scrolled near its top (or too short to scroll). */
export function maybeLoadOlder(app: App, log: HTMLElement | null) {
  const s = app.store.getState();
  const id = s.ui.current;
  const b = id ? s.chat.bots[id] : undefined;
  if (!id || !b || !b.hasOlder || b.loadingOlder || !b.firstSeq || isLocked(s.bots.byId[id])) return;
  if (log && log.scrollTop > 300) return;
  if (app.send({ type: "older", bot: id, before: b.firstSeq, limit: PAGE })) app.dispatch(local("chat/olderAsked", { bot: id }));
}

/** Asks the home computer for a report; away from it, shows the copy kept here. */
export function openReport(app: App, bot: string, id: string) {
  const key = reportKey(bot, id);
  app.dispatch(local("chat/wantReport", { key }));
  const s = app.store.getState();
  if (s.connection.status === "open" && app.send({ type: "open_report", bot, id, utc_offset_min: -new Date().getTimezoneOffset() })) return;
  if (s.chat.reports[key]) {
    app.dispatch(local("chat/wantReport", { key: null }));
    app.dispatch(local("ui/open", { dialog: "report", arg: key }));
    return;
  }
  app.toast(app.phone ? "Opens when you're back on the home Wi-Fi." : "Not connected. The report opens once oxbot is back.", true);
}

// ---------- files
export const MB = 1024 * 1024;
const MAX_FILES = 10;
const MAX_FILE = 25 * MB;
const MAX_VIDEO = 200 * MB;
export const MAX_TEXT = 1000000;
const VIDEO = /\.(mov|mp4|m4v)$/i;
export const PHONE_FILES = "Sending files from a phone isn't possible yet. Send them from the home computer.";

export function sizeText(n: number): string {
  return n >= MB ? `${(n / MB).toFixed(n >= 10 * MB ? 0 : 1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`;
}

/** Uploads in flight, by chip key, so a removed chip stops its upload. */
const xhrs = new Map<number, XMLHttpRequest>();

export function addFiles(app: App, list: FileList | File[] | null) {
  const files = [...(list ?? [])];
  const s = app.store.getState();
  const bot = s.ui.current;
  if (!bot || !files.length) return;
  // A phone or laptop reaches the home computer only through its app.
  if (app.phone) { app.toast(PHONE_FILES, true); return; }
  if (!s.bots.byId[bot] || isLocked(s.bots.byId[bot])) return;
  for (const file of files) {
    const chips = app.store.getState().chat.drafts[bot] ?? [];
    if (chips.filter((c) => c.state !== "error").length >= MAX_FILES) { app.toast(`oxbot takes up to ${MAX_FILES} files per message.`, true); break; }
    const name = file.name || "pasted file";
    const video = VIDEO.test(name);
    const cap = video ? MAX_VIDEO : MAX_FILE;
    const big = file.size > cap;
    app.dispatch(local("chat/addChip", {
      chip: {
        bot, name, size: file.size, progress: 0, att: null,
        state: big ? "error" : "uploading",
        error: big ? `${name} is ${sizeText(file.size)}; oxbot takes ${video ? "videos" : "files"} up to ${cap / MB} MB.` : "",
      },
    }));
    if (!big) upload(app, bot, app.store.getState().chat.chipSeq, name, file);
  }
}

function upload(app: App, bot: string, key: number, name: string, file: File) {
  const as = new URLSearchParams(location.search).get("as");
  const q = new URLSearchParams({ bot, name });
  if (as) q.set("as", as);
  const xhr = new XMLHttpRequest();
  xhrs.set(key, xhr);
  xhr.open("POST", `/upload?${q}`);
  // The home computer's key goes in a header, not the address.
  const farmKey = takeKey(location, sessionStore(), () => {});
  if (farmKey && !as) xhr.setRequestHeader("X-Oxbot-Key", farmKey);
  const patch = (p: Record<string, unknown>) => app.dispatch(local("chat/chip", { bot, key, patch: p }));
  xhr.upload.onprogress = (e) => { if (e.lengthComputable) patch({ progress: e.loaded / e.total }); };
  const failed = (why: string) => { xhrs.delete(key); patch({ state: "error", error: why }); };
  xhr.onload = () => {
    let body: { id?: string; error?: string } | null = null;
    try { body = JSON.parse(xhr.responseText); } catch { body = null; }
    if (xhr.status === 200 && body?.id) { xhrs.delete(key); patch({ state: "ready", att: body }); return; }
    failed(body?.error || xhr.responseText || "The file didn't upload.");
  };
  xhr.onerror = () => failed("The file didn't upload: oxbot isn't reachable.");
  xhr.send(file);
}

export function removeChip(app: App, bot: string, key: number) {
  const chip = (app.store.getState().chat.drafts[bot] ?? []).find((c) => c.key === key);
  if (!chip) return;
  xhrs.get(key)?.abort();
  xhrs.delete(key);
  // Uploaded but not sent: the home computer deletes it.
  if (chip.state === "ready" && chip.att) app.send({ type: "drop_upload", bot, id: chip.att.id });
  app.dispatch(local("chat/removeChip", { bot, key }));
}

/** Sends the message box with its files. */
export function sendMessage(app: App) {
  const s = app.store.getState();
  const bot = s.ui.current;
  if (!bot) return;
  const text = s.chat.input.trim();
  const chips = s.chat.drafts[bot] ?? [];
  if (uploading(s.chat, bot)) { app.toast("Wait until the files finish uploading."); return; }
  const files = chips.filter((c) => c.state === "ready" && c.att).map((c) => (c.att as { id: string }).id);
  if (!text && !files.length) return;
  if (text.length > MAX_TEXT) { app.toast("That message is too long to send (over 1 MB). Attach it as a file instead.", true); return; }
  stopDictation(app);
  // Offline: the message stays in the box, to send once oxbot is back.
  if (s.connection.status !== "open" || !app.send({ type: "send", bot, text, files })) {
    app.toast("Not connected yet. Your message is still here: send it once you're back on.", true);
    return;
  }
  app.dispatch(local("chat/sent", { bot, text }));
}

// ---------- dictation: the home computer's microphone, through the server
export function startDictation(app: App, box: HTMLTextAreaElement | null) {
  const s = app.store.getState();
  if (!s.connection.settings?.dictation || s.chat.dictating) return;
  const value = s.chat.input;
  let before = box ? value.slice(0, box.selectionStart) : value;
  const after = box ? value.slice(box.selectionEnd) : "";
  if (before && !/\s$/.test(before)) before += " ";
  if (!app.send({ type: "dictate", on: true })) { app.toast("Not connected to oxbot.", true); return; }
  app.dispatch(local("chat/dictating", { at: { before, after } }));
}

export function stopDictation(app: App) {
  if (!app.store.getState().chat.dictating) return;
  app.send({ type: "dictate", on: false });
  app.dispatch(local("chat/dictating", { at: null }));
}

/** Settings → General, where the AI account is (the banner's and the note's buttons). */
export function openEngines(app: App) {
  app.dispatch(local("ui/side", { open: false }));
  app.dispatch(local("ui/open", { dialog: "settings", arg: "setGeneral" }));
}

export type { ChatState };
