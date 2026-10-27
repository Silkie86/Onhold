// ⌘K: search records across the workspace and jump to pages.

import { Command } from "cmdk";
import { Dialog as D } from "radix-ui";
import { type ReactNode, useEffect, useState } from "react";

export interface PaletteHit {
  id: string;
  title: string;
  detail?: string | null;
  group: string;
}

export interface PaletteAction {
  id: string;
  title: string;
  group: string;
  icon?: ReactNode;
}

export function CommandPalette({
  open,
  onOpenChange,
  search,
  actions,
  onSelect,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  search: (q: string) => Promise<PaletteHit[]>;
  actions: PaletteAction[];
  onSelect: (item: PaletteHit | PaletteAction) => void;
}) {
  const [q, setQ] = useState("");
  const [hits, setHits] = useState<PaletteHit[]>([]);

  useEffect(() => {
    if (!open) setQ("");
  }, [open]);
  useEffect(() => {
    if (!q.trim()) {
      setHits([]);
      return;
    }
    let live = true;
    const t = setTimeout(() => {
      search(q)
        .then((h) => live && setHits(h))
        .catch(() => live && setHits([]));
    }, 150);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [q, search]);

  const groups = [...new Set(hits.map((h) => h.group))];
  const actionGroups = [...new Set(actions.map((a) => a.group))];
  const choose = (item: PaletteHit | PaletteAction) => {
    onOpenChange(false);
    onSelect(item);
  };

  return (
    <D.Root open={open} onOpenChange={onOpenChange}>
      <D.Portal>
        <D.Overlay className="fixed inset-0 z-40 bg-black/40" />
        <D.Content className="fixed top-[15vh] left-1/2 z-50 w-[calc(100vw-2rem)] max-w-xl -translate-x-1/2 overflow-hidden rounded-lg border bg-card shadow-lg">
          <D.Title className="sr-only">Search and go</D.Title>
          <D.Description className="sr-only">Search records or jump to a page</D.Description>
          <Command shouldFilter={false} label="Search and go">
            <Command.Input
              value={q}
              onValueChange={setQ}
              placeholder="Search contacts, organisations, talent… or go to a page"
              className="h-12 w-full border-b bg-transparent px-4 text-sm outline-none"
            />
            <Command.List className="max-h-80 overflow-y-auto p-1">
              <Command.Empty className="p-4 text-center text-sm text-muted-foreground">No matches.</Command.Empty>
              {groups.map((g) => (
                <Command.Group key={g} heading={g} className="text-xs text-muted-foreground [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1">
                  {hits
                    .filter((h) => h.group === g)
                    .map((h) => (
                      <Command.Item
                        key={`${h.group}-${h.id}`}
                        value={`${h.group}-${h.id}`}
                        onSelect={() => choose(h)}
                        className="flex cursor-default flex-col rounded-sm px-2 py-1.5 text-sm text-foreground data-[selected=true]:bg-accent"
                      >
                        <span>{h.title}</span>
                        {h.detail && <span className="text-xs text-muted-foreground">{h.detail}</span>}
                      </Command.Item>
                    ))}
                </Command.Group>
              ))}
              {actionGroups.map((g) => (
                <Command.Group key={g} heading={g} className="text-xs text-muted-foreground [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1">
                  {actions
                    .filter((a) => a.group === g && (!q || a.title.toLowerCase().includes(q.toLowerCase())))
                    .map((a) => (
                      <Command.Item
                        key={a.id}
                        value={a.id}
                        onSelect={() => choose(a)}
                        className="flex cursor-default items-center gap-2 rounded-sm px-2 py-1.5 text-sm text-foreground data-[selected=true]:bg-accent"
                      >
                        {a.icon}
                        {a.title}
                      </Command.Item>
                    ))}
                </Command.Group>
              ))}
            </Command.List>
          </Command>
        </D.Content>
      </D.Portal>
    </D.Root>
  );
}
