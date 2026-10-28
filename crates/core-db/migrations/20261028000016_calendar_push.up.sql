-- M6, third part continued: bookings in members' own calendars. A member
-- connects their Google or Microsoft calendar; OnHold makes a calendar of
-- its own in that account ("OnHold · <workspace>") and keeps the
-- workspace's bookings (or one talent's) in it as events: holds and
-- pending bookings as tentative, confirmed ones as busy. Fees never go.

-- A member's calendar that bookings are written into. The grant is
-- encrypted with the server's keyring and bound to the row's id.
CREATE TABLE calendar_pushes (
    id              uuid PRIMARY KEY,
    workspace_id    uuid NOT NULL REFERENCES workspaces (id),
    -- Whose calendar it is; it carries only bookings they may see.
    user_id         uuid NOT NULL REFERENCES users (id),
    -- One talent's bookings, or every booking when NULL.
    talent_id       uuid,
    provider        text NOT NULL CHECK (provider IN ('google', 'microsoft')),
    -- The account's address, lower-cased.
    account         text NOT NULL CHECK (length(account) BETWEEN 3 AND 320 AND account = lower(account)),
    -- Emptied once the grant is revoked or no longer needed.
    secret          bytea NOT NULL,
    -- The calendar OnHold made in that account, once made.
    remote_calendar text CHECK (length(remote_calendar) <= 1000),
    -- active: working. failing: being retried. reconnect: only connecting
    -- again helps (the grant was withdrawn, or OnHold's calendar deleted).
    status          text NOT NULL DEFAULT 'active'
                        CHECK (status IN ('active', 'failing', 'reconnect')),
    failures        integer NOT NULL DEFAULT 0 CHECK (failures >= 0),
    last_error      text CHECK (length(last_error) <= 1000),
    last_error_at   timestamptz,
    last_synced_at  timestamptz,
    -- When every booking is next compared with the calendar (bookings that
    -- change are written straight away in between).
    next_sync_at    timestamptz,
    sync_lease_until timestamptz,
    -- Names the write holding the lease; each of its steps checks it, so a
    -- write that lost the connection (disconnected, or its lease taken
    -- after running out) stops.
    sync_token      uuid,
    -- Disconnected along with others (a member leaving, a workspace
    -- closing): the worker removes OnHold's calendar and revokes the grant,
    -- then empties it.
    revoke_at       timestamptz,
    connected_at    timestamptz NOT NULL DEFAULT now(),
    disconnected_at timestamptz,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    version         bigint NOT NULL DEFAULT 1,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id),
    CHECK (disconnected_at IS NULL OR next_sync_at IS NULL)
);
-- One live connection per member, account and choice of bookings.
CREATE UNIQUE INDEX calendar_pushes_live ON calendar_pushes
    (workspace_id, user_id, provider, account, coalesce(talent_id, '00000000-0000-0000-0000-000000000000'))
    WHERE disconnected_at IS NULL;
CREATE INDEX calendar_pushes_due ON calendar_pushes (next_sync_at) WHERE next_sync_at IS NOT NULL;
CREATE INDEX calendar_pushes_revoke ON calendar_pushes (revoke_at) WHERE revoke_at IS NOT NULL;
CREATE TRIGGER calendar_pushes_updated_at BEFORE UPDATE ON calendar_pushes
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER calendar_pushes_version BEFORE UPDATE ON calendar_pushes
    FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Which event in that calendar stands for which booking, and a hash of
-- what was written, so an unchanged booking is not written again. A copy
-- of the calendar's state: rows go when their events do.
CREATE TABLE calendar_push_events (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    push_id      uuid NOT NULL,
    booking_id   uuid NOT NULL,
    remote_id    text NOT NULL CHECK (length(remote_id) BETWEEN 1 AND 1000),
    content_hash bytea NOT NULL,
    written_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (push_id, booking_id),
    FOREIGN KEY (workspace_id, push_id) REFERENCES calendar_pushes (workspace_id, id)
);

-- Revoking a Google grant ends every grant OnHold holds for that account,
-- so a mailbox, a talent's calendar and a member's calendar all count.
CREATE OR REPLACE FUNCTION integration_account_in_use(p_provider text, p_account text, p_except uuid)
    RETURNS boolean
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT EXISTS (
            SELECT 1 FROM integrations
            WHERE provider = p_provider AND account = p_account AND id <> p_except
              AND disconnected_at IS NULL
        ) OR EXISTS (
            SELECT 1 FROM talent_calendars
            WHERE provider = p_provider AND account = p_account AND id <> p_except
              AND disconnected_at IS NULL
        ) OR EXISTS (
            SELECT 1 FROM calendar_pushes
            WHERE provider = p_provider AND account = p_account AND id <> p_except
              AND (disconnected_at IS NULL OR revoke_at IS NOT NULL)
        )
    $$;

ALTER TABLE calendar_pushes ENABLE ROW LEVEL SECURITY;
ALTER TABLE calendar_push_events ENABLE ROW LEVEL SECURITY;

CREATE POLICY calendar_pushes_tenant ON calendar_pushes FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY calendar_push_events_tenant ON calendar_push_events FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
-- The worker's sweep finds calendars due a full comparison or a revoke (ids
-- and times only); the job runs inside the workspace as the app role.
CREATE POLICY calendar_pushes_worker ON calendar_pushes FOR SELECT TO onhold_worker
    USING (next_sync_at IS NOT NULL OR revoke_at IS NOT NULL);

GRANT SELECT, INSERT, UPDATE ON calendar_pushes TO onhold_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON calendar_push_events TO onhold_app;
GRANT SELECT (id, workspace_id, next_sync_at, sync_lease_until, revoke_at) ON calendar_pushes TO onhold_worker;
