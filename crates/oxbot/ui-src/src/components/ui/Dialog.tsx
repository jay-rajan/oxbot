// A native <dialog>, opened as a modal while `open` is true. Escape (or the
// browser closing it) calls onClose. Children are drawn only while open, so
// each opening starts from the current state.
//
// Inside, a DialogForm (or DialogFrame) lays it out: a header with the
// title and a close button, a body that scrolls, and a footer that stays
// in view with the buttons.
import { type FormEvent, forwardRef, type ReactNode, useEffect, useRef } from "react";
import { cx } from "./cx";
import { IconButton } from "./Button";
import { CloseIcon } from "./icons";
import "./dialog.css";

export function Dialog({
  id,
  open,
  onClose,
  className,
  labelledBy,
  size = "md",
  children,
}: {
  id: string;
  open: boolean;
  onClose: () => void;
  className?: string;
  labelledBy?: string;
  /** sm 400, md 480, lg 640, xl 860; full fills a phone. */
  size?: "sm" | "md" | "lg" | "xl";
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
    <dialog id={id} ref={ref} className={cx("dlg", `dlg-${size}`, className)} aria-labelledby={labelledBy}>
      {open ? children : null}
    </dialog>
  );
}

type FrameProps = {
  title: ReactNode;
  titleId?: string;
  /** Shown as a ✕ in the header when given. */
  onClose?: () => void;
  closeId?: string;
  /** Extra things in the header, before the ✕. */
  headerExtra?: ReactNode;
  footer?: ReactNode;
  className?: string;
  bodyClassName?: string;
  children: ReactNode;
};

function Head({ title, titleId, onClose, closeId, headerExtra }: Pick<FrameProps, "title" | "titleId" | "onClose" | "closeId" | "headerExtra">) {
  return (
    <header className="dlg-head">
      <h2 id={titleId} className="dlg-title">
        {title}
      </h2>
      {headerExtra}
      {onClose ? (
        <IconButton label="Close" id={closeId} onClick={onClose} className="dlg-close">
          <CloseIcon />
        </IconButton>
      ) : null}
    </header>
  );
}

/** A dialog laid out as a form: Enter submits, the footer holds its buttons.
 *  Its ref is the <form> (e.g. to tell whether focus is inside it). */
export const DialogForm = forwardRef<
  HTMLFormElement,
  FrameProps & { id?: string; onSubmit: (ev: FormEvent<HTMLFormElement>) => void }
>(function DialogForm({ id, onSubmit, ...frame }, ref) {
  return (
    <form ref={ref} id={id} className={cx("dlg-frame", frame.className)} onSubmit={onSubmit}>
      <Head {...frame} />
      <div className={cx("dlg-body", frame.bodyClassName)}>{frame.children}</div>
      {frame.footer ? <footer className="dlg-foot">{frame.footer}</footer> : null}
    </form>
  );
});

/** A dialog that isn't a form (a report, Settings). */
export function DialogFrame({ id, ...frame }: FrameProps & { id?: string }) {
  return (
    <div id={id} className={cx("dlg-frame", frame.className)}>
      <Head {...frame} />
      <div className={cx("dlg-body", frame.bodyClassName)}>{frame.children}</div>
      {frame.footer ? <footer className="dlg-foot">{frame.footer}</footer> : null}
    </div>
  );
}
