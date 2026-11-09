// Choices. Segmented picks one of a few (real radio inputs underneath, so
// forms and assistive tools read them as radios). Checkbox and Switch are a
// box or a toggle with its text beside it.
import { type InputHTMLAttributes, type ReactNode } from "react";
import { cx } from "./cx";
import "./choice.css";

export function Segmented<T extends string>({
  name,
  label,
  options,
  value,
  onChange,
  disabled,
  id,
  className,
}: {
  name: string;
  label: string;
  options: readonly (readonly [T, ReactNode])[];
  value: T;
  onChange: (v: T) => void;
  disabled?: boolean;
  id?: string;
  className?: string;
}) {
  return (
    <div className={cx("segmented", className)} role="radiogroup" aria-label={label} id={id}>
      {options.map(([v, text]) => (
        <label key={v} className="seg-option">
          <input type="radio" name={name} value={v} checked={value === v} disabled={disabled} onChange={() => onChange(v)} />
          <span>{text}</span>
        </label>
      ))}
    </div>
  );
}

type BoxProps = Omit<InputHTMLAttributes<HTMLInputElement>, "type"> & { children: ReactNode; rowId?: string; rowHidden?: boolean };

export function Checkbox({ children, className, rowId, rowHidden, ...rest }: BoxProps) {
  return (
    <label className={cx("check", className)} id={rowId} hidden={rowHidden}>
      <input type="checkbox" {...rest} />
      <span className="check-text">{children}</span>
    </label>
  );
}

export function Switch({ children, className, rowId, rowHidden, ...rest }: BoxProps) {
  return (
    <label className={cx("switch", className)} id={rowId} hidden={rowHidden}>
      <span className="check-text">{children}</span>
      <input type="checkbox" role="switch" {...rest} />
    </label>
  );
}
