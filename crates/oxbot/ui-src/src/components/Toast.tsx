// The toast: one line at the bottom, shown inside the open dialog (so it's
// above the dialog's backdrop), else on the page. Hides after 9 s (7 s for
// an error).
import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { useApp, useSel } from "../app/context";
import { local } from "../state/store";

export function Toast() {
  const { dispatch } = useApp();
  const toast = useSel((s) => s.ui.toast);
  const dialog = useSel((s) => s.ui.dialog);
  const [host, setHost] = useState<Element>(document.body);
  useEffect(() => {
    // After the dialog's own render: the topmost open one.
    const find = () => setHost([...document.querySelectorAll("dialog[open]")].pop() ?? document.body);
    find();
    const t = setTimeout(find, 0);
    return () => clearTimeout(t);
  }, [dialog, toast]);
  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => dispatch(local("ui/hideToast", { n: toast.n })), toast.error ? 7000 : 9000);
    return () => clearTimeout(t);
  }, [toast, dispatch]);
  return createPortal(
    <div className={`toast${toast?.error ? " error" : ""}`} id="toast" hidden={!toast}>
      {toast?.text}
    </div>,
    host,
  );
}
