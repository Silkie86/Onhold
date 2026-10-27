// Who is signed in, and in which workspace they are working.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createContext, useContext } from "react";
import { ApiFailure, type Me, api, currentWorkspace, ok, setCsrfToken, setWorkspace } from "./api.ts";
import { outbox, wipe, wipeAllButQueue } from "./offline/store.ts";
import { isNetworkError } from "./offline/sync.ts";
import { IS_APP, KEEPS_OFFLINE_COPY, appToken, saveAppToken } from "./platform.ts";

// The apps and the installed website remember who was signed in, so they can open offline.
const ME_KEY = "onhold.me";

function rememberedMe(): Me | null {
  try {
    const raw = localStorage.getItem(ME_KEY);
    return raw ? (JSON.parse(raw) as Me) : null;
  } catch {
    return null;
  }
}

function remember(me: Me | null) {
  try {
    if (me) localStorage.setItem(ME_KEY, JSON.stringify(me));
    else localStorage.removeItem(ME_KEY);
  } catch {
    // Not remembered: the app needs a connection to open next time.
  }
}

export const meQuery = {
  queryKey: ["me"],
  queryFn: async (): Promise<Me | null> => {
    if (IS_APP && !appToken()) return null;
    try {
      const me = await ok(api.GET("/api/v1/auth/me"));
      setCsrfToken(me.session.csrf_token);
      const ids = me.workspaces.map((w) => w.id);
      const stored = currentWorkspace();
      if (!stored || !ids.includes(stored)) setWorkspace(ids[0]);
      if (KEEPS_OFFLINE_COPY) remember(me);
      return me;
    } catch (e) {
      if (e instanceof ApiFailure && e.status === 401) {
        setCsrfToken(undefined);
        if (IS_APP) await saveAppToken(null);
        if (KEEPS_OFFLINE_COPY) {
          // Expired, or signed out from another device: the workspace's data
          // leaves this one. Unsent changes wait for the same person's next sign-in.
          remember(null);
          await wipeAllButQueue().catch(() => undefined);
        }
        return null;
      }
      if (KEEPS_OFFLINE_COPY && isNetworkError(e)) {
        const known = rememberedMe();
        if (known) {
          // The website's changes carry the session's form token; it is still good.
          setCsrfToken(known.session.csrf_token);
          return known;
        }
      }
      throw e;
    }
  },
  staleTime: 5 * 60 * 1000,
};

export function useMe() {
  return useQuery(meQuery);
}

export interface Session {
  me: Me;
  workspace: Me["workspaces"][number];
  /** Switches workspace and refetches everything for it. */
  switchWorkspace: (id: string) => void;
  signOut: () => Promise<void>;
}

export const SessionContext = createContext<Session | null>(null);

/** The signed-in session; only inside the app shell. */
export function useSession(): Session {
  const s = useContext(SessionContext);
  if (!s) throw new Error("useSession outside the signed-in app");
  return s;
}

export function useSessionActions(me: Me) {
  const qc = useQueryClient();
  const workspace = me.workspaces.find((w) => w.id === currentWorkspace()) ?? me.workspaces[0];
  return {
    workspace,
    switchWorkspace: (id: string) => {
      setWorkspace(id);
      qc.removeQueries({ predicate: (q) => q.queryKey[0] !== "me" });
      void qc.invalidateQueries({ queryKey: ["me"] });
    },
    signOut: async () => {
      if (KEEPS_OFFLINE_COPY) {
        const waiting = (await Promise.all(me.workspaces.map((w) => outbox(w.id)))).flat().length;
        const unsent = `${waiting} ${waiting === 1 ? "change has" : "changes have"} not been sent yet and will be lost.`;
        if (waiting > 0 && !window.confirm(`${unsent} Sign out anyway?`)) return;
      }
      await api.POST("/api/v1/auth/logout").catch(() => undefined);
      setCsrfToken(undefined);
      if (IS_APP) await saveAppToken(null);
      if (KEEPS_OFFLINE_COPY) {
        remember(null);
        // Nothing of the workspace stays on the device.
        await wipe();
      }
      qc.clear();
      window.location.assign("/sign-in");
    },
  };
}
