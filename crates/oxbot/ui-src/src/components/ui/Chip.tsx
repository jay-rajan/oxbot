// A chip: a small rounded label. Static, a button (e.g. a file to open), or
// with a ✕ to remove it. Tone "bad" for a problem. Other attributes
// (data-*, aria-*) go on the chip itself.
import { type HTMLAttributes, type ReactNode } from "react";
import { cx } from "./cx";
import "./chip.css";

export function Chip({
  children,
  sub,
  tone,
  onClick,
  onRemove,
  removeLabel = "Remove",
  className,
  title,
  lead,
  ...rest
}: Omit<HTMLAttributes<HTMLElement>, "title" | "onClick" | "children" | "className"> & {
  children: ReactNode;
  sub?: ReactNode;
  tone?: "bad" | "accent" | "ok";
  onClick?: () => void;
  onRemove?: () => void;
  removeLabel?: string;
  className?: string;
  title?: string;
  lead?: ReactNode;
}) {
  const inner = (
    <>
      {lead}
      <span className="nm">{children}</span>
      {sub ? <small>{sub}</small> : null}
    </>
  );
  const cls = cx("chip", tone, className);
  if (onClick)
    return (
      <button type="button" {...rest} className={cls} title={title} onClick={onClick}>
        {inner}
      </button>
    );
  return (
    <span {...rest} className={cx(cls, onRemove && "removable")} title={title}>
      {inner}
      {onRemove ? (
        <button type="button" className="x" aria-label={removeLabel} onClick={onRemove}>
          ✕
        </button>
      ) : null}
    </span>
  );
}

/** A tiny count or label in a pill (e.g. 3 waiting). */
export function Pill({ children, tone, className }: { children: ReactNode; tone?: "accent" | "ok" | "warn" | "danger" | undefined; className?: string }) {
  return <span className={cx("pill", tone, className)}>{children}</span>;
}
