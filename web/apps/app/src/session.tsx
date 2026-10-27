// Who is signed in, and in which workspace they are working.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createContext, useContext } from "react";
import { ApiFailure, type Me, api, currentWorkspace, ok, setCsrfToken, setWorkspace } from "./api.ts";

export const meQuery = {
  queryKey: ["me"],
  queryFn: async (): Promise<Me | null> => {
    try {
      const me = await ok(api.GET("/api/v1/auth/me"));
      setCsrfToken(me.session.csrf_token);
      const ids = me.workspaces.map((w) => w.id);
      const stored = currentWorkspace();
      if (!stored || !ids.includes(stored)) setWorkspace(ids[0]);
      return me;
    } catch (e) {
      if (e instanceof ApiFailure && e.status === 401) {
        setCsrfToken(undefined);
        return null;
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
      await api.POST("/api/v1/auth/logout");
      setCsrfToken(undefined);
      qc.clear();
      window.location.assign("/sign-in");
    },
  };
}
