// The message box: file chips, attach, the box itself, the microphone, Stop
// and Send. Enter sends; the box grows with its text.
import { useEffect, useLayoutEffect, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import type { BotInfo } from "../../net/protocol";
import type { Chip } from "../../state/chat";
import { local } from "../../state/store";
import { PHONE_FILES, addFiles, isLocked, removeChip, sendMessage, sizeText, startDictation, stopDictation } from "./actions";

const MIC_TITLE = "Dictate with the home computer's microphone (the audio stays on the home computer)";
const NO_CHIPS: Chip[] = [];

function ChipView({ c }: { c: Chip }) {
  const app = useApp();
  // The server's message names the file itself, mostly.
  const name = c.state !== "error" ? c.name : c.error.includes(c.name) ? c.error : `${c.name}: ${c.error}`;
  const small =
    c.state === "sending" ? "sending…" : c.state === "uploading" ? (c.progress >= 1 ? "checking…" : `${Math.round(c.progress * 100)}%`) : c.att?.note || sizeText(c.att?.size ?? c.size);
  return (
    <div className={`chip${c.state === "error" ? " bad" : ""}`} data-state={c.state}>
      {c.state === "uploading" || c.state === "sending" ? <span className="spin" aria-hidden="true" /> : null}
      <span className="nm" title={c.name}>{name}</span>
      {c.state !== "error" ? <small>{small}</small> : null}
      <button type="button" className="x" aria-label={`Remove ${c.name}`} hidden={c.state === "sending"} onClick={() => removeChip(app, c.bot, c.key)}>✕</button>
    </div>
  );
}

export function Composer({ info, busy, uploadingNow }: { info: BotInfo | undefined; busy: boolean; uploadingNow: boolean }) {
  const app = useApp();
  const current = useSel((s) => s.ui.current);
  const input = useSel((s) => s.chat.input);
  const dictating = useSel((s) => !!s.chat.dictating);
  const caret = useSel((s) => s.chat.caret);
  const chips = useSel((s) => (s.ui.current ? s.chat.drafts[s.ui.current] : undefined)) ?? NO_CHIPS;
  const micOk = useSel((s) => !!s.connection.settings?.dictation);
  const connected = useSel((s) => s.connection.status === "open");
  const box = useRef<HTMLTextAreaElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const form = useRef<HTMLFormElement>(null);
  const locked = isLocked(info);

  // The box grows with its text, up to 40% of the window.
  useLayoutEffect(() => {
    const t = box.current;
    if (!t) return;
    t.style.height = "auto";
    t.style.height = `${Math.min(t.scrollHeight, window.innerHeight * 0.4)}px`;
  }, [input]);

  // Dictation put text in: the cursor goes after it.
  useEffect(() => {
    const t = box.current;
    if (!t || !caret) return;
    t.setSelectionRange(caret.at, caret.at);
    if (caret.focus) t.focus();
  }, [caret]);

  // A bot opened: the cursor goes in the box.
  useEffect(() => { if (current) box.current?.focus(); }, [current]);

  const known = !!info;
  return (
    <>
      <form
        className="composer"
        id="composer"
        ref={form}
        onSubmit={(ev) => { ev.preventDefault(); sendMessage(app); }}
      >
        <div className="chips" id="chips" aria-live="polite">
          {current ? chips.map((c) => <ChipView key={c.key} c={c} />) : null}
        </div>
        <button
          className="icon-btn"
          id="attach"
          type="button"
          aria-label="Attach files"
          title="Attach files (you can also drop or paste them)"
          disabled={known && locked}
          onClick={() => { if (app.phone) { app.toast(PHONE_FILES, true); return; } fileInput.current?.click(); }}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"><path d="M20.5 11.5 12.6 19.4a5 5 0 0 1-7.1-7.1l8.3-8.3a3.4 3.4 0 0 1 4.8 4.8l-8.3 8.3a1.7 1.7 0 0 1-2.4-2.4l7.6-7.6" /></svg>
        </button>
        <input
          type="file"
          id="fileInput"
          className="file-input"
          multiple
          tabIndex={-1}
          aria-hidden="true"
          ref={fileInput}
          onChange={(ev) => { addFiles(app, ev.currentTarget.files); ev.currentTarget.value = ""; }}
        />
        <textarea
          id="input"
          rows={1}
          ref={box}
          placeholder={known && locked ? "Private" : dictating ? "Listening…" : "Message"}
          aria-label="Message"
          disabled={known && locked}
          value={input}
          onChange={(ev) => {
            // Typing takes over from dictation.
            stopDictation(app);
            app.dispatch(local("chat/input", { text: ev.currentTarget.value }));
          }}
          onPaste={(ev) => {
            const files = ev.clipboardData?.files;
            if (files?.length) { ev.preventDefault(); addFiles(app, files); }
          }}
          onKeyDown={(ev) => {
            if (ev.key === "Escape" && dictating) { ev.preventDefault(); stopDictation(app); return; }
            if (ev.key === "Enter" && !ev.shiftKey && !ev.nativeEvent.isComposing) { ev.preventDefault(); form.current?.requestSubmit(); }
          }}
        />
        <button
          className={`icon-btn${dictating ? " on" : ""}`}
          id="mic"
          type="button"
          aria-label="Dictate"
          aria-pressed={dictating}
          title={dictating ? "Stop dictating" : MIC_TITLE}
          hidden={!known || locked || !micOk}
          onClick={() => (dictating ? stopDictation(app) : startDictation(app, box.current))}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round"><rect x="9" y="3" width="6" height="11" rx="3" /><path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21" /></svg>
        </button>
        <button className="ghost" id="stop" type="button" hidden={!known || !busy || locked} onClick={() => current && app.send({ type: "cancel", bot: current })}>Stop</button>
        <button className="primary" id="send" type="submit" disabled={known && (busy || locked || !connected || uploadingNow)}>Send</button>
      </form>
    </>
  );
}
