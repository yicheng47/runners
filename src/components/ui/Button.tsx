import type { ButtonHTMLAttributes, ReactNode } from "react";

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md";

const base =
  "inline-flex cursor-pointer items-center justify-center gap-1.5 rounded font-medium transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-1 focus-visible:ring-offset-bg disabled:opacity-50 disabled:pointer-events-none disabled:cursor-not-allowed";

const variants: Record<Variant, string> = {
  primary:
    "bg-accent text-accent-ink focus-visible:ring-accent",
  secondary:
    "bg-raised text-fg border border-line-strong focus-visible:ring-line-strong",
  ghost:
    "bg-transparent text-fg-2 focus-visible:ring-line-strong",
  danger:
    "bg-transparent text-danger border border-danger/40 focus-visible:ring-danger",
};

const sizes: Record<Size, string> = {
  sm: "text-xs px-2.5 py-1",
  md: "text-sm px-3 py-1.5",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  children?: ReactNode;
}

export function Button({
  variant = "secondary",
  size = "md",
  className = "",
  type = "button",
  children,
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={`${base} ${variants[variant]} ${sizes[size]} ${className}`}
      {...rest}
    >
      {children}
    </button>
  );
}
