import { Check, Copy, LoaderCircle } from "lucide-react";
import { type ComponentProps, type ReactNode, useEffect, useState } from "react";
import { cn } from "../lib.ts";

export function Badge({ className, ...props }: ComponentProps<"span">) {
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-full border bg-secondary px-2 py-0.5 text-xs font-medium text-secondary-foreground",
        className,
      )}
      {...props}
    />
  );
}

export function Card({ className, ...props }: ComponentProps<"div">) {
  return <div className={cn("rounded-lg border bg-card text-card-foreground shadow-xs", className)} {...props} />;
}

export function Spinner({ className, label = "Loading" }: { className?: string; label?: string }) {
  return <LoaderCircle role="status" aria-label={label} className={cn("size-4 animate-spin", className)} />;
}

export function Alert({
  tone = "error",
  className,
  children,
}: {
  tone?: "error" | "info" | "success";
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      role={tone === "error" ? "alert" : "status"}
      className={cn(
        "rounded-md border px-3 py-2 text-sm",
        tone === "error" && "border-destructive/40 bg-destructive/10 text-destructive",
        tone === "info" && "bg-muted text-foreground",
        tone === "success" && "border-success/40 bg-success/10",
        className,
      )}
    >
      {children}
    </div>
  );
}

export function EmptyState({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2 px-4 py-12 text-center">
      <p className="font-medium">{title}</p>
      {children && <div className="text-sm text-muted-foreground">{children}</div>}
    </div>
  );
}

/** The colours a status dot can take, drawn from the theme. */
const DOT: Record<string, string> = {
  green: "bg-success",
  amber: "bg-warning",
  red: "bg-destructive",
  grey: "bg-muted-foreground",
  blue: "bg-info",
  purple: "bg-violet-500",
};

/** A coloured dot before a label, e.g. a status. Unknown colours draw grey. */
export function StatusDot({ colour, label, className }: { colour?: string | null; label?: ReactNode; className?: string }) {
  return (
    <span className={cn("inline-flex items-center gap-1.5", className)}>
      <span aria-hidden className={cn("inline-block size-2.5 shrink-0 rounded-full", DOT[colour ?? ""] ?? DOT.grey)} />
      {label}
    </span>
  );
}

/** A small button that copies `value`, and says so for a moment. */
export function CopyButton({ value, label, className }: { value: string; label: string; className?: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const t = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(t);
  }, [copied]);
  return (
    <button
      type="button"
      className={cn(
        "inline-flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-accent-foreground focus-visible:outline-2 focus-visible:outline-ring",
        className,
      )}
      aria-label={copied ? `Copied ${label}` : `Copy ${label}`}
      title={copied ? "Copied" : "Copy"}
      onClick={() => void navigator.clipboard?.writeText(value).then(() => setCopied(true), () => undefined)}
    >
      {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
    </button>
  );
}
