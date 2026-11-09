// A banner across a view: offline, an alert, something to set up. One line
// of text, an optional action at the end.
import { type ReactNode } from "react";
import { cx } from "./cx";
import "./banner.css";

export function Banner({
  tone = "info",
  id,
  hidden,
  className,
  lead,
  actions,
  role,
  children,
}: {
  tone?: "info" | "warn" | "danger";
  id?: string;
  hidden?: boolean;
  className?: string;
  lead?: ReactNode;
  actions?: ReactNode;
  role?: string;
  children: ReactNode;
}) {
  return (
    <div className={cx("banner", `banner-${tone}`, className)} id={id} hidden={hidden} role={role}>
      {lead ? <span className="banner-lead">{lead}</span> : null}
      <div className="banner-text">{children}</div>
      {actions ? <div className="banner-actions">{actions}</div> : null}
    </div>
  );
}
