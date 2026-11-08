// A research report, over the chat. The report is a whole page the home
// computer made safe; it's shown in a sandboxed frame (no scripts, no access
// to this page), set as the frame's srcdoc, never as HTML here. The frame
// stays in the page while closed (empty), as the tests read it.
import { useEffect, useRef } from "react";
import { useApp, useSel } from "../../app/context";
import { splitReportKey } from "../../state/chat";
import { local } from "../../state/store";
import { openReport } from "./actions";

export function ReportDialog() {
  const app = useApp();
  const { dispatch } = app;
  const dialog = useSel((s) => s.ui.dialog);
  const arg = useSel((s) => s.ui.dialogArg);
  const ready = useSel((s) => s.chat.reportReady);
  const report = useSel((s) => (s.ui.dialog === "report" && s.ui.dialogArg ? s.chat.reports[s.ui.dialogArg] : undefined));
  const ref = useRef<HTMLDialogElement>(null);
  const frame = useRef<HTMLIFrameElement>(null);
  const open = dialog === "report" && !!report;

  // The report asked for came: show it.
  useEffect(() => {
    if (!ready) return;
    dispatch(local("chat/reportShown"));
    dispatch(local("ui/open", { dialog: "report", arg: ready }));
  }, [ready, dispatch]);

  // Opened from elsewhere (a bot's report list) before it's here: ask for it.
  useEffect(() => {
    if (dialog !== "report" || !arg || report) return;
    dispatch(local("ui/close", { dialog: "report" }));
    const k = splitReportKey(arg);
    if (k) openReport(app, k.bot, k.id);
  }, [dialog, arg, report, app, dispatch]);

  useEffect(() => {
    const d = ref.current;
    const f = frame.current;
    if (!d || !f) return;
    // Opened first, then filled: a frame filled while its dialog is closed
    // can stay blank in Chrome. srcdoc is a property, so the HTML never
    // passes through this page's parser.
    if (open && !d.open) d.showModal();
    if (!open && d.open) d.close();
    f.srcdoc = open && report ? report.html : "";
  }, [open, report]);

  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    const closed = () => {
      if (frame.current) frame.current.srcdoc = "";
      dispatch(local("ui/close", { dialog: "report" }));
    };
    d.addEventListener("close", closed);
    return () => d.removeEventListener("close", closed);
  }, [dispatch]);

  return (
    <dialog id="reportDlg" className="report-view" aria-labelledby="reportTitle" ref={ref}>
      <header className="report-head">
        <button className="ghost" type="button" id="reportBack" onClick={() => ref.current?.close()}>← Back</button>
        <h2 id="reportTitle">{report?.title ?? "Report"}</h2>
      </header>
      <iframe id="reportFrame" ref={frame} title="Report" sandbox="allow-popups allow-popups-to-escape-sandbox" referrerPolicy="no-referrer" />
    </dialog>
  );
}
