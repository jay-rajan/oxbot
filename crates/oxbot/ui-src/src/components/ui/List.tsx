// Sections and lists. A Section is a small caps title over its content.
// A List holds ListRows: something at the start (a dot, an icon), a title
// with an optional second line, and actions at the end.
import { type HTMLAttributes, type ReactNode } from "react";
import { cx } from "./cx";
import "./list.css";

export function Section({
  title,
  titleId,
  actions,
  className,
  id,
  hidden,
  children,
}: {
  title?: ReactNode;
  titleId?: string;
  actions?: ReactNode;
  className?: string;
  id?: string;
  hidden?: boolean;
  children: ReactNode;
}) {
  return (
    <section className={cx("section", className)} id={id} hidden={hidden} aria-labelledby={title ? titleId : undefined}>
      {title || actions ? (
        <div className="section-head">
          {title ? (
            <h3 className="section-title" id={titleId}>
              {title}
            </h3>
          ) : null}
          {actions ? <div className="section-actions">{actions}</div> : null}
        </div>
      ) : null}
      {children}
    </section>
  );
}

export function List({ className, children, ...rest }: HTMLAttributes<HTMLUListElement>) {
  return (
    <ul className={cx("list", className)} {...rest}>
      {children}
    </ul>
  );
}

export function ListRow({
  lead,
  title,
  sub,
  trail,
  className,
  onClick,
  children,
  ...rest
}: Omit<HTMLAttributes<HTMLLIElement>, "title"> & {
  lead?: ReactNode;
  title: ReactNode;
  sub?: ReactNode;
  trail?: ReactNode;
}) {
  return (
    <li className={cx("list-row", !!onClick && "list-row-click", !lead && "no-lead", className)} onClick={onClick} {...rest}>
      {lead ? <span className="list-lead">{lead}</span> : null}
      <span className="list-main name">
        <span className="list-title">{title}</span>
        {sub ? <small className="list-sub">{sub}</small> : null}
      </span>
      {trail ? <span className="list-trail">{trail}</span> : null}
      {children}
    </li>
  );
}

/** A short empty or status line inside a list or section. */
export function Note({ id, hidden, children }: { id?: string; hidden?: boolean; children: ReactNode }) {
  return (
    <p className="note-text" id={id} hidden={hidden}>
      {children}
    </p>
  );
}
