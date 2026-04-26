import type { InputHTMLAttributes, ReactNode, TextareaHTMLAttributes } from "react";

export function Label({
  htmlFor,
  children,
  hint,
}: {
  htmlFor: string;
  children: ReactNode;
  hint?: ReactNode;
}) {
  return (
    <label
      htmlFor={htmlFor}
      className="flex items-baseline justify-between text-xs font-medium text-fg-2"
    >
      <span>{children}</span>
      {hint ? <span className="text-fg-3">{hint}</span> : null}
    </label>
  );
}

const inputBase =
  "w-full rounded border border-line-strong bg-bg px-2.5 py-1.5 text-sm text-fg placeholder:text-fg-3 focus:outline-none focus:border-fg-3 disabled:opacity-60";

export function Input(props: InputHTMLAttributes<HTMLInputElement>) {
  const { className = "", ...rest } = props;
  return <input className={`${inputBase} ${className}`} {...rest} />;
}

export function Textarea(props: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  const { className = "", ...rest } = props;
  return <textarea className={`${inputBase} font-mono ${className}`} {...rest} />;
}

export function FieldError({ children }: { children?: ReactNode }) {
  if (!children) return null;
  return <p className="text-xs text-danger">{children}</p>;
}

export function Field({
  id,
  label,
  hint,
  error,
  children,
}: {
  id: string;
  label: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1">
      <Label htmlFor={id} hint={hint}>
        {label}
      </Label>
      {children}
      <FieldError>{error}</FieldError>
    </div>
  );
}
