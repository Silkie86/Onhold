// Sends the person elsewhere once, when rendered. The router's own
// `<Navigate>` navigates again on every render of its parent, and a parent
// that re-renders while that navigation is still pending (the apps answer
// "who is signed in" without a network round trip) never lets it finish.

import { useNavigate } from "@tanstack/react-router";
import { useEffect, useRef } from "react";

export function GoTo({ to, redirect }: { to: "/sign-in" | "/auth/mfa"; redirect: string }) {
  const navigate = useNavigate();
  const gone = useRef(false);
  useEffect(() => {
    if (gone.current) return;
    gone.current = true;
    void navigate({ to, search: { redirect }, replace: true });
  }, [navigate, to, redirect]);
  return null;
}
