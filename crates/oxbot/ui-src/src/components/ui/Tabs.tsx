// Tabs: a row (or a column) of choices that switch what's below. The panes
// are the screen's own; each has role="tabpanel".
import { type ReactNode } from "react";
import { cx } from "./cx";
import "./tabs.css";

export function Tabs<T extends string>({
  label,
  tabs,
  value,
  onChange,
  vertical,
  className,
}: {
  label: string;
  /** [value, text, button id?] */
  tabs: readonly (readonly [T, ReactNode, string?])[];
  value: T;
  onChange: (v: T) => void;
  vertical?: boolean;
  className?: string;
}) {
  return (
    <div
      className={cx("tabs", vertical && "tabs-vertical", className)}
      role="tablist"
      aria-label={label}
      aria-orientation={vertical ? "vertical" : "horizontal"}
    >
      {tabs.map(([v, text, id]) => (
        <button key={v} type="button" role="tab" id={id} data-tab={v} aria-selected={value === v} onClick={() => onChange(v)}>
          {text}
        </button>
      ))}
    </div>
  );
}
