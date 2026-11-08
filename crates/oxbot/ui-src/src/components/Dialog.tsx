// A native <dialog>, opened as a modal while `open` is true. Escape (or the
// browser closing it) calls onClose. Children are drawn only while open, so
// each opening starts from the current state.
import { type ReactNode, useEffect, useRef } from "react";

export function Dialog({
  id,
  open,
  onClose,
  className,
  labelledBy,
  children,
}: {
  id: string;
  open: boolean;
  onClose: () => void;
  className?: string;
  labelledBy?: string;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) d.showModal();
    if (!open && d.open) d.close();
  }, [open]);
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    const closed = () => onClose();
    d.addEventListener("close", closed);
    return () => d.removeEventListener("close", closed);
  }, [onClose]);
  return (
    <dialog id={id} ref={ref} className={className} aria-labelledby={labelledBy}>
      {open ? children : null}
    </dialog>
  );
}
