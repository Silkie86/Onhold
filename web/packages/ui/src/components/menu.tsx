import { DropdownMenu as M } from "radix-ui";
import type { ComponentProps, ReactNode } from "react";
import { cn } from "../lib.ts";

export const Menu = M.Root;
export const MenuTrigger = M.Trigger;

export function MenuContent({ className, align = "end", ...props }: ComponentProps<typeof M.Content>) {
  return (
    <M.Portal>
      <M.Content
        align={align}
        sideOffset={4}
        className={cn("z-50 min-w-44 rounded-md border bg-card p-1 text-sm shadow-md", className)}
        {...props}
      />
    </M.Portal>
  );
}

export function MenuItem({ className, ...props }: ComponentProps<typeof M.Item>) {
  return (
    <M.Item
      className={cn(
        "flex cursor-default items-center gap-2 rounded-sm px-2 py-1.5 outline-none select-none data-[disabled]:opacity-50 data-[highlighted]:bg-accent",
        className,
      )}
      {...props}
    />
  );
}

export function MenuCheckboxItem({
  children,
  className,
  ...props
}: ComponentProps<typeof M.CheckboxItem> & { children: ReactNode }) {
  return (
    <M.CheckboxItem
      className={cn(
        "flex cursor-default items-center gap-2 rounded-sm px-2 py-1.5 outline-none select-none data-[highlighted]:bg-accent",
        className,
      )}
      {...props}
    >
      <span className="flex size-4 items-center justify-center rounded border">
        <M.ItemIndicator>✓</M.ItemIndicator>
      </span>
      {children}
    </M.CheckboxItem>
  );
}

export function MenuLabel({ className, ...props }: ComponentProps<typeof M.Label>) {
  return <M.Label className={cn("px-2 py-1 text-xs text-muted-foreground", className)} {...props} />;
}

export function MenuSeparator() {
  return <M.Separator className="my-1 h-px bg-border" />;
}
