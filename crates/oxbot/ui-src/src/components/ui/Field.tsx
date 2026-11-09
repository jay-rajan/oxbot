// A form field: a label above its control, then one line of hint or an
// error. Input, TextArea and Select are the controls, styled alike.
import { forwardRef, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes, type TextareaHTMLAttributes } from "react";
import { cx } from "./cx";
import "./field.css";

export function Field({
  label,
  hint,
  error,
  className,
  id,
  hidden,
  children,
}: {
  label: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  className?: string;
  /** The field's wrapper id (some screens show and hide whole fields). */
  id?: string;
  hidden?: boolean;
  children: ReactNode;
}) {
  return (
    <div className={cx("field", className)} id={id} hidden={hidden}>
      <label className="field-label">
        <span className="field-name">{label}</span>
        {children}
      </label>
      {error ? <p className="field-error" role="alert">{error}</p> : hint ? <p className="field-hint">{hint}</p> : null}
    </div>
  );
}

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function Input({ className, ...rest }, ref) {
  return <input ref={ref} className={cx("input", className)} {...rest} />;
});

export const TextArea = forwardRef<HTMLTextAreaElement, TextareaHTMLAttributes<HTMLTextAreaElement>>(function TextArea(
  { className, ...rest },
  ref,
) {
  return <textarea ref={ref} className={cx("input", "textarea", className)} {...rest} />;
});

export const Select = forwardRef<HTMLSelectElement, SelectHTMLAttributes<HTMLSelectElement>>(function Select(
  { className, ...rest },
  ref,
) {
  return <select ref={ref} className={cx("input", "select", className)} {...rest} />;
});

/** A one-line hint outside a Field (e.g. under a group of choices). */
export function Hint({ id, hidden, children }: { id?: string; hidden?: boolean; children: ReactNode }) {
  return (
    <p className="field-hint" id={id} hidden={hidden}>
      {children}
    </p>
  );
}

/** An error line outside a Field. */
export function ErrorText({ id, hidden, children }: { id?: string; hidden?: boolean; children: ReactNode }) {
  return (
    <p className="field-error" id={id} hidden={hidden} role="alert">
      {children}
    </p>
  );
}
