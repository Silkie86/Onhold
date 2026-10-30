// The choice to keep recent email on this device, for reading and answering
// it without a connection, and how much room it takes. Only in the apps and
// the installed website, which keep an offline copy at all.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Alert, Card, Checkbox } from "@onhold/ui";
import { useEffect, useState } from "react";
import { useOffline } from "../data.ts";
import { MAIL_DAYS, MAIL_MAX_BYTES, formatBytes } from "../offline/mail.ts";
import type { OfflineSync } from "../offline/sync.ts";

export function KeepMailCard() {
  const sync = useOffline();
  return sync ? <KeepMail sync={sync} /> : null;
}

function KeepMail({ sync }: { sync: OfflineSync }) {
  const qc = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  /** The choice as just made, shown straight away while the device saves it. */
  const [chosen, setChosen] = useState<boolean | null>(null);
  const state = useQuery({
    queryKey: ["mail-device", sync.ws],
    queryFn: async () => ({ on: await sync.mail.enabled(), ...(await sync.mail.usage()) }),
  });
  // What is kept changes as the device syncs.
  useEffect(() => sync.onRecordsChanged(() => void qc.invalidateQueries({ queryKey: ["mail-device", sync.ws] })), [sync, qc]);

  const toggle = async (on: boolean) => {
    setError(null);
    setChosen(on);
    try {
      await sync.mail.setEnabled(on);
      await qc.invalidateQueries({ queryKey: ["mail-device", sync.ws] });
      setChosen(null);
      if (on) {
        await sync.syncNow();
        await qc.invalidateQueries({ queryKey: ["mail-device", sync.ws] });
      }
    } catch {
      setChosen(null);
      setError("This device could not change that. Try again.");
    }
  };

  const s = state.data;
  const on = chosen ?? s?.on ?? false;
  return (
    <Card className="flex flex-col gap-2 p-4 text-sm">
      <label className="flex items-start gap-2">
        <Checkbox className="mt-0.5" checked={on} disabled={!s} onChange={(e) => void toggle(e.target.checked)} />
        <span>
          <span className="font-medium">Keep recent email on this device</span>
          <span className="block text-muted-foreground">
            Read and answer the last {MAIL_DAYS / 7} weeks of conversations without a connection. Text only, without
            attachments, and never more than {formatBytes(MAIL_MAX_BYTES)}. Replies wait on the device and send when
            you are back online.
          </span>
        </span>
      </label>
      {on && s?.on && (
        <p className="text-muted-foreground">
          {s.threads === 0
            ? "Nothing kept yet. Recent conversations are saved the next time this device syncs."
            : `Using ${formatBytes(s.bytes)} for ${s.threads} ${s.threads === 1 ? "conversation" : "conversations"}.`}
        </p>
      )}
      {error && <Alert>{error}</Alert>}
    </Card>
  );
}
