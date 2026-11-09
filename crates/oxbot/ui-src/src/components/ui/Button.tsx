// Buttons. One shape, four kinds:
//   primary   the one main action of a view (red); at most one per view
//   secondary the other actions (outlined)
//   quiet     low-key actions in rows and headers (no edge)
//   danger    destructive actions (red text, outlined)
// IconButton is a square quiet button with an icon and a required label.
import { type ButtonHTMLAttributes, forwardRef, type ReactNode } from "react";
import { cx } from "./cx";
import "./button.css";

export type ButtonKind = "primary" | "secondary" | "quiet" | "danger";

type Props = ButtonHTMLAttributes<HTMLButtonElement> & {
  kind?: ButtonKind;
  size?: "md" | "sm";
  icon?: ReactNode;
};

export const Button = forwardRef<HTMLButtonElement, Props>(function Button(
  { kind = "secondary", size = "md", icon, className, type = "button", children, ...rest },
  ref,
) {
  return (
    <button ref={ref} type={type} className={cx("btn", `btn-${kind}`, size === "sm" && "btn-sm", className)} {...rest}>
      {icon ? <span className="btn-icon" aria-hidden="true">{icon}</span> : null}
      {children}
    </button>
  );
});

type IconProps = ButtonHTMLAttributes<HTMLButtonElement> & { label: string; pressed?: boolean; size?: "md" | "sm" };

export const IconButton = forwardRef<HTMLButtonElement, IconProps>(function IconButton(
  { label, pressed, size = "md", className, type = "button", children, title, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      aria-label={label}
      title={title ?? label}
      aria-pressed={pressed}
      className={cx("icon-button", size === "sm" && "icon-button-sm", className)}
      {...rest}
    >
      {children}
    </button>
  );
});
