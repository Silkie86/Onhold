import { Popover as P } from "radix-ui";
import type { ComponentProps } from "react";
import { cn } from "../lib.ts";

export const Popover = P.Root;
export const PopoverTrigger = P.Trigger;
/** What the popover lines up with, when that is not the trigger itself. */
export const PopoverAnchor = P.Anchor;

/** A small panel over the page, as wide as its anchor; closes on a tap elsewhere, Escape or focus leaving it. */
export function PopoverContent({ className, ...props }: ComponentProps<typeof P.Content>) {
  return (
    <P.Portal>
      <P.Content
        sideOffset={4}
        collisionPadding={16}
        className={cn(
          "z-50 w-[var(--radix-popover-trigger-width)] max-w-[calc(100vw-2rem)] rounded-md border bg-card p-3 text-sm text-card-foreground shadow-lg outline-none",
          className,
        )}
        {...props}
      />
    </P.Portal>
  );
}
