-- M6, third part: talent's own calendars, busy times only. Someone at the
-- agency sends a talent (or each member of a band) a link; they connect
-- their Google or Microsoft calendar, or paste a private iCal link, with no
-- OnHold account. The worker reads when they are busy every few minutes,
-- never what they are doing, and clash checks count it like availability.

-- A link a talent opens to connect their calendar. Only its hash is kept.
CREATE TABLE calendar_connect_links (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    talent_id    uuid NOT NULL,
    token_hash   bytea NOT NULL UNIQUE,
    created_by   uuid NOT NULL REFERENCES users (id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    revoked_at   timestamptz,
    last_used_at timestamptz,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id)
);
CREATE INDEX calendar_connect_links_talent ON calendar_connect_links (workspace_id, talent_id)
    WHERE revoked_at IS NULL;

-- A calendar connected for a talent. The grant (or the iCal link, which is
-- itself a secret) is encrypted with the server's keyring and bound to the
-- row's id.
CREATE TABLE talent_calendars (
    id              uuid PRIMARY KEY,
    workspace_id    uuid NOT NULL REFERENCES workspaces (id),
    talent_id       uuid NOT NULL,
    -- The link it was connected through.
    link_id         uuid NOT NULL,
    provider        text NOT NULL CHECK (provider IN ('google', 'microsoft', 'ical')),
    -- Whose it is: the account's address, or the iCal link's host.
    account         text NOT NULL CHECK (length(account) BETWEEN 1 AND 320),
    -- A band member's name, as they gave it.
    person          text CHECK (length(btrim(person)) BETWEEN 1 AND 100),
    secret          bytea NOT NULL,
    -- active: working. failing: being retried. reconnect: only connecting
    -- it again helps (the grant was withdrawn, the link stopped working).
    status          text NOT NULL DEFAULT 'active'
                        CHECK (status IN ('active', 'failing', 'reconnect')),
    failures        integer NOT NULL DEFAULT 0 CHECK (failures >= 0),
    last_error      text CHECK (length(last_error) <= 1000),
    last_error_at   timestamptz,
    last_synced_at  timestamptz,
    next_poll_at    timestamptz,
    poll_lease_until timestamptz,
    connected_at    timestamptz NOT NULL DEFAULT now(),
    disconnected_at timestamptz,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    version         bigint NOT NULL DEFAULT 1,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id),
    FOREIGN KEY (workspace_id, link_id) REFERENCES calendar_connect_links (workspace_id, id),
    CHECK (disconnected_at IS NULL OR next_poll_at IS NULL)
);
CREATE INDEX talent_calendars_talent ON talent_calendars (workspace_id, talent_id)
    WHERE disconnected_at IS NULL;
CREATE INDEX talent_calendars_due ON talent_calendars (next_poll_at)
    WHERE next_poll_at IS NOT NULL;
CREATE TRIGGER talent_calendars_updated_at BEFORE UPDATE ON talent_calendars
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER talent_calendars_version BEFORE UPDATE ON talent_calendars
    FOR EACH ROW EXECUTE FUNCTION bump_version();

-- When a connected calendar is busy, from yesterday to a year ahead: times
-- only, no titles. A copy of the provider's answer, replaced as it changes
-- (so it never grows); the calendar's row and the audit log keep the history.
CREATE TABLE calendar_busy (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    calendar_id  uuid NOT NULL,
    talent_id    uuid NOT NULL,
    starts_at    timestamptz NOT NULL,
    ends_at      timestamptz NOT NULL,
    tentative    boolean NOT NULL DEFAULT false,
    CHECK (ends_at > starts_at),
    FOREIGN KEY (workspace_id, calendar_id) REFERENCES talent_calendars (workspace_id, id),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id)
);
CREATE INDEX calendar_busy_talent ON calendar_busy
    USING gist (workspace_id, talent_id, tstzrange(starts_at, ends_at, '[)'));
CREATE INDEX calendar_busy_calendar ON calendar_busy (calendar_id, starts_at);

-- The link behind a token, for the public page, which has no session.
CREATE FUNCTION calendar_connect_link_find(p_token_hash bytea)
    RETURNS TABLE (id uuid, workspace_id uuid, talent_id uuid)
    LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        UPDATE calendar_connect_links l SET last_used_at = now()
        WHERE l.token_hash = p_token_hash AND l.revoked_at IS NULL AND l.expires_at > now()
          AND EXISTS (SELECT 1 FROM memberships m
                      WHERE m.workspace_id = l.workspace_id AND m.user_id = l.created_by
                        AND m.deleted_at IS NULL)
        RETURNING l.id, l.workspace_id, l.talent_id
    $$;
REVOKE ALL ON FUNCTION calendar_connect_link_find(bytea) FROM PUBLIC;

ALTER TABLE calendar_connect_links ENABLE ROW LEVEL SECURITY;
ALTER TABLE talent_calendars ENABLE ROW LEVEL SECURITY;
ALTER TABLE calendar_busy ENABLE ROW LEVEL SECURITY;

CREATE POLICY calendar_connect_links_tenant ON calendar_connect_links FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY talent_calendars_tenant ON talent_calendars FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY calendar_busy_tenant ON calendar_busy FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
-- The worker's sweep finds calendars due a read (ids and times only); the
-- read runs inside the workspace as the app role.
CREATE POLICY talent_calendars_worker ON talent_calendars FOR SELECT TO onhold_worker
    USING (next_poll_at IS NOT NULL);

GRANT EXECUTE ON FUNCTION calendar_connect_link_find(bytea) TO onhold_app;
GRANT SELECT, INSERT, UPDATE ON calendar_connect_links, talent_calendars TO onhold_app;
GRANT SELECT, INSERT, DELETE ON calendar_busy TO onhold_app;
GRANT SELECT (id, workspace_id, next_poll_at, poll_lease_until) ON talent_calendars TO onhold_worker;
