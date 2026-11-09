// A card: a bordered block that stands apart (an approval waiting for
// someone, a sign-in, a schedule to confirm). Tone colors its edge.
import { type HTMLAttributes, type ReactNode } from "react";
import { cx } from "./cx";
import "./card.css";

export function Card({
  tone = "plain",
  title,
  className,
  children,
  ...rest
}: Omit<HTMLAttributes<HTMLDivElement>, "title"> & {
  tone?: "plain" | "warn" | "danger";
  title?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className={cx("card", `card-${tone}`, className)} {...rest}>
      {title ? <h3 className="card-title">{title}</h3> : null}
      {children}
    </div>
  );
}
