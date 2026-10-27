-- M4: bookings and calendar. Enquiries move through the workspace's enquiry
-- pipeline; a hold is a booking with status `hold` and an expiry; quotes are
-- priced offers on a hold that the buyer accepts on the web; availability
-- marks when talent cannot (or only can) work; tasks are to-dos on any
-- record; calendar feeds publish bookings to other calendars by secret link.
--
-- Same conventions as the CRM: UUIDv7 ids, a version, soft deletes, `custom`
-- and `tags`, and links that carry the workspace in their foreign keys.
--
-- Double booking is refused by the database itself: `booking_talent` keeps
-- a copy of each booking's time and whether it is confirmed, and an
-- exclusion constraint allows no two confirmed bookings of one talent to
-- overlap, whatever the application does and however requests race. An
-- audited override (by an admin) takes a booking out of the constraint.

-- Range types with equality on uuid, for the exclusion constraint.
CREATE EXTENSION IF NOT EXISTS btree_gist;

-- Permissions -----------------------------------------------------------------

INSERT INTO role_permissions (workspace_id, role, permission)
SELECT w.id, r.role::member_role, r.permission
FROM workspaces w
CROSS JOIN (VALUES
    ('owner', 'enquiry:read'), ('admin', 'enquiry:read'), ('agent', 'enquiry:read'),
    ('finance', 'enquiry:read'), ('viewer', 'enquiry:read'),
    ('owner', 'enquiry:write'), ('admin', 'enquiry:write'), ('agent', 'enquiry:write'),
    ('owner', 'booking:read'), ('admin', 'booking:read'), ('agent', 'booking:read'),
    ('finance', 'booking:read'), ('viewer', 'booking:read'),
    ('owner', 'booking:write'), ('admin', 'booking:write'), ('agent', 'booking:write'),
    ('owner', 'booking:override'), ('admin', 'booking:override'),
    ('owner', 'quote:read'), ('admin', 'quote:read'), ('agent', 'quote:read'),
    ('finance', 'quote:read'), ('viewer', 'quote:read'),
    ('owner', 'quote:write'), ('admin', 'quote:write'), ('agent', 'quote:write'),
    ('owner', 'task:read'), ('admin', 'task:read'), ('agent', 'task:read'),
    ('finance', 'task:read'), ('viewer', 'task:read'),
    ('owner', 'task:write'), ('admin', 'task:write'), ('agent', 'task:write'),
    ('finance', 'task:write')
) AS r (role, permission)
ON CONFLICT DO NOTHING;

-- Workspace settings ------------------------------------------------------------

-- How long a new hold lasts before it expires by itself; 0 means never.
ALTER TABLE workspaces ADD COLUMN hold_days integer NOT NULL DEFAULT 7 CHECK (hold_days BETWEEN 0 AND 365);
GRANT UPDATE (hold_days) ON workspaces TO onhold_app;

-- Numbering ------------------------------------------------------------------------

-- Per-workspace counters (quote numbers now, invoice numbers in M5). Taken
-- with UPDATE … RETURNING inside the creating transaction, so numbers have
-- no gaps and are never reused.
CREATE TABLE workspace_counters (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    name         text NOT NULL CHECK (name ~ '^[a-z_]{1,40}$'),
    value        bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (workspace_id, name)
);

-- Enquiries -------------------------------------------------------------------------

CREATE TABLE enquiries (
    id               uuid PRIMARY KEY,
    workspace_id     uuid NOT NULL REFERENCES workspaces (id),
    -- What is asked for: "Summer party", "Headline slot, Saturday".
    title            text NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    source           text NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'email', 'form', 'portal')),
    organisation_id  uuid,
    contact_id       uuid,
    venue_id         uuid,
    -- A venue not (yet) in the CRM.
    venue_text       text CHECK (length(venue_text) <= 200),
    starts_on        date,
    ends_on          date,
    budget_minor     bigint CHECK (budget_minor >= 0),
    budget_currency  char(3) CHECK (budget_currency ~ '^[A-Z]{3}$'),
    pipeline_id      uuid REFERENCES pipelines (id),
    stage_id         uuid REFERENCES pipeline_stages (id),
    stage_entered_at timestamptz,
    owner_id         uuid REFERENCES users (id),
    lost_reason      text CHECK (length(lost_reason) <= 500),
    notes            text CHECK (length(notes) <= 20000),
    tags             text[] NOT NULL DEFAULT '{}',
    custom           jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom) = 'object'),
    version          bigint NOT NULL DEFAULT 1,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    created_by       uuid REFERENCES users (id),
    updated_by       uuid REFERENCES users (id),
    deleted_at       timestamptz,
    search_text      text GENERATED ALWAYS AS (lower(
        title || ' ' || coalesce(venue_text, '') || ' ' || search_join(tags))) STORED,
    UNIQUE (workspace_id, id),
    CHECK ((budget_minor IS NULL) = (budget_currency IS NULL)),
    CHECK (ends_on IS NULL OR (starts_on IS NOT NULL AND ends_on >= starts_on)),
    FOREIGN KEY (workspace_id, organisation_id) REFERENCES organisations (workspace_id, id),
    FOREIGN KEY (workspace_id, contact_id) REFERENCES contacts (workspace_id, id),
    FOREIGN KEY (workspace_id, venue_id) REFERENCES organisations (workspace_id, id)
);
CREATE INDEX enquiries_workspace ON enquiries (workspace_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX enquiries_stage ON enquiries (workspace_id, stage_id) WHERE deleted_at IS NULL;
CREATE INDEX enquiries_search ON enquiries USING gin (search_text gin_trgm_ops);
CREATE INDEX enquiries_custom ON enquiries USING gin (custom jsonb_path_ops);
CREATE INDEX enquiries_organisation ON enquiries (workspace_id, organisation_id) WHERE deleted_at IS NULL;
CREATE INDEX enquiries_contact ON enquiries (workspace_id, contact_id) WHERE deleted_at IS NULL;
CREATE TRIGGER enquiries_updated_at BEFORE UPDATE ON enquiries FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER enquiries_version BEFORE UPDATE ON enquiries FOR EACH ROW EXECUTE FUNCTION bump_version();

-- The talent an enquiry asks for, in order.
CREATE TABLE enquiry_talent (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    enquiry_id   uuid NOT NULL,
    talent_id    uuid NOT NULL,
    position     integer NOT NULL DEFAULT 0,
    PRIMARY KEY (enquiry_id, talent_id),
    FOREIGN KEY (workspace_id, enquiry_id) REFERENCES enquiries (workspace_id, id),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id)
);
CREATE INDEX enquiry_talent_talent ON enquiry_talent (workspace_id, talent_id);

-- Bookings ------------------------------------------------------------------------

CREATE TYPE booking_status AS ENUM ('hold', 'pending', 'confirmed', 'cancelled', 'completed', 'expired');

CREATE TABLE bookings (
    id                uuid PRIMARY KEY,
    workspace_id      uuid NOT NULL REFERENCES workspaces (id),
    enquiry_id        uuid,
    title             text NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    status            booking_status NOT NULL DEFAULT 'hold',
    status_changed_at timestamptz NOT NULL DEFAULT now(),
    organisation_id   uuid,
    contact_id        uuid,
    venue_id          uuid,
    venue_text        text CHECK (length(venue_text) <= 200),
    -- The IANA zone the times are shown and entered in (the venue's).
    timezone          text NOT NULL CHECK (length(timezone) BETWEEN 1 AND 64),
    starts_at         timestamptz NOT NULL,
    ends_at           timestamptz NOT NULL,
    all_day           boolean NOT NULL DEFAULT false,
    -- Load-in, soundcheck, set times: [{label, at}].
    schedule          jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(schedule) = 'array'),
    -- When a hold lapses by itself.
    hold_expires_at   timestamptz,
    fee_minor         bigint CHECK (fee_minor >= 0),
    fee_currency      char(3) CHECK (fee_currency ~ '^[A-Z]{3}$'),
    -- Hundredths of a percent of the fee due as a deposit: 5000 is 50%.
    deposit_bps       integer CHECK (deposit_bps BETWEEN 0 AND 10000),
    cancel_reason     text CHECK (length(cancel_reason) <= 500),
    -- Confirmed despite a clash, by an admin, for a reason. Audited.
    clash_override    boolean NOT NULL DEFAULT false,
    override_reason   text CHECK (length(override_reason) <= 500),
    override_by       uuid REFERENCES users (id),
    notes             text CHECK (length(notes) <= 20000),
    tags              text[] NOT NULL DEFAULT '{}',
    custom            jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom) = 'object'),
    version           bigint NOT NULL DEFAULT 1,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    created_by        uuid REFERENCES users (id),
    updated_by        uuid REFERENCES users (id),
    deleted_at        timestamptz,
    search_text       text GENERATED ALWAYS AS (lower(
        title || ' ' || coalesce(venue_text, '') || ' ' || search_join(tags))) STORED,
    UNIQUE (workspace_id, id),
    CHECK (ends_at > starts_at),
    CHECK ((fee_minor IS NULL) = (fee_currency IS NULL)),
    CHECK (NOT clash_override OR override_reason IS NOT NULL),
    FOREIGN KEY (workspace_id, enquiry_id) REFERENCES enquiries (workspace_id, id),
    FOREIGN KEY (workspace_id, organisation_id) REFERENCES organisations (workspace_id, id),
    FOREIGN KEY (workspace_id, contact_id) REFERENCES contacts (workspace_id, id),
    FOREIGN KEY (workspace_id, venue_id) REFERENCES organisations (workspace_id, id)
);
CREATE INDEX bookings_workspace_time ON bookings (workspace_id, starts_at) WHERE deleted_at IS NULL;
CREATE INDEX bookings_status ON bookings (workspace_id, status) WHERE deleted_at IS NULL;
CREATE INDEX bookings_enquiry ON bookings (workspace_id, enquiry_id) WHERE deleted_at IS NULL;
CREATE INDEX bookings_organisation ON bookings (workspace_id, organisation_id) WHERE deleted_at IS NULL;
CREATE INDEX bookings_search ON bookings USING gin (search_text gin_trgm_ops);
CREATE INDEX bookings_custom ON bookings USING gin (custom jsonb_path_ops);
-- The worker's sweep: holds to expire, bookings to complete.
CREATE INDEX bookings_hold_expiry ON bookings (hold_expires_at) WHERE status = 'hold' AND deleted_at IS NULL;
CREATE INDEX bookings_confirmed_end ON bookings (ends_at) WHERE status = 'confirmed' AND deleted_at IS NULL;
CREATE TRIGGER bookings_updated_at BEFORE UPDATE ON bookings FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER bookings_version BEFORE UPDATE ON bookings FOR EACH ROW EXECUTE FUNCTION bump_version();

-- The talent (people, acts, rooms, kit) a booking takes, each checked for clashes.
CREATE TABLE booking_talent (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    booking_id   uuid NOT NULL,
    talent_id    uuid NOT NULL,
    position     integer NOT NULL DEFAULT 0,
    -- Copies of the booking's time and firmness, kept in step by triggers.
    during       tstzrange NOT NULL,
    firm         boolean NOT NULL DEFAULT false,
    PRIMARY KEY (booking_id, talent_id),
    FOREIGN KEY (workspace_id, booking_id) REFERENCES bookings (workspace_id, id),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id),
    CONSTRAINT booking_talent_no_double_booking
        EXCLUDE USING gist (workspace_id WITH =, talent_id WITH =, during WITH &&) WHERE (firm)
);
CREATE INDEX booking_talent_talent ON booking_talent USING gist (workspace_id, talent_id, during);

-- Confirmed, not overridden and not deleted: what may not overlap.
CREATE FUNCTION booking_is_firm(b bookings) RETURNS boolean
    LANGUAGE sql IMMUTABLE
    AS $$ SELECT b.status = 'confirmed' AND NOT b.clash_override AND b.deleted_at IS NULL $$;

CREATE FUNCTION booking_talent_fill() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    b bookings;
BEGIN
    SELECT * INTO b FROM bookings WHERE id = NEW.booking_id;
    NEW.during := tstzrange(b.starts_at, b.ends_at, '[)');
    NEW.firm := booking_is_firm(b);
    RETURN NEW;
END;
$$;
CREATE TRIGGER booking_talent_fill BEFORE INSERT ON booking_talent
    FOR EACH ROW EXECUTE FUNCTION booking_talent_fill();

CREATE FUNCTION bookings_sync_talent() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    UPDATE booking_talent
    SET during = tstzrange(NEW.starts_at, NEW.ends_at, '[)'), firm = booking_is_firm(NEW)
    WHERE booking_id = NEW.id;
    RETURN NULL;
END;
$$;
CREATE TRIGGER bookings_sync_talent AFTER UPDATE OF starts_at, ends_at, status, clash_override, deleted_at
    ON bookings FOR EACH ROW EXECUTE FUNCTION bookings_sync_talent();

-- Availability ----------------------------------------------------------------------

CREATE TYPE availability_kind AS ENUM ('unavailable', 'tentative', 'available_only');

CREATE TABLE availability (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    talent_id    uuid NOT NULL,
    kind         availability_kind NOT NULL DEFAULT 'unavailable',
    starts_at    timestamptz NOT NULL,
    ends_at      timestamptz NOT NULL,
    all_day      boolean NOT NULL DEFAULT false,
    source       text NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'portal', 'external')),
    note         text CHECK (length(note) <= 500),
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    deleted_at   timestamptz,
    CHECK (ends_at > starts_at),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id)
);
CREATE INDEX availability_talent ON availability USING gist (workspace_id, talent_id, tstzrange(starts_at, ends_at, '[)'))
    WHERE deleted_at IS NULL;
CREATE TRIGGER availability_updated_at BEFORE UPDATE ON availability FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER availability_version BEFORE UPDATE ON availability FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Quotes ----------------------------------------------------------------------------

CREATE TYPE quote_status AS ENUM ('draft', 'sent', 'viewed', 'accepted', 'declined', 'expired', 'withdrawn');

CREATE TABLE quotes (
    id                 uuid PRIMARY KEY,
    workspace_id       uuid NOT NULL REFERENCES workspaces (id),
    -- The hold (or booking) it prices.
    booking_id         uuid NOT NULL,
    -- Q-<number>; a revision keeps the number and counts up.
    number             bigint NOT NULL,
    revision           integer NOT NULL DEFAULT 1 CHECK (revision >= 1),
    -- The revision this one replaces.
    supersedes_id      uuid,
    status             quote_status NOT NULL DEFAULT 'draft',
    currency           char(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    -- [{description, quantity, unit_minor, talent_id?}]
    line_items         jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(line_items) = 'array'),
    total_minor        bigint NOT NULL DEFAULT 0 CHECK (total_minor >= 0),
    valid_until        date,
    -- The note to the buyer, and the terms they accept.
    message            text CHECK (length(message) <= 5000),
    terms              text CHECK (length(terms) <= 20000),
    -- The secret link's hash; set when first sent.
    token_hash         bytea UNIQUE,
    sent_at            timestamptz,
    sent_to            text CHECK (length(sent_to) <= 320),
    viewed_at          timestamptz,
    decided_at         timestamptz,
    decided_name       text CHECK (length(decided_name) <= 200),
    decided_email      text CHECK (length(decided_email) <= 320),
    decided_ip         inet,
    decided_user_agent text CHECK (length(decided_user_agent) <= 500),
    decline_reason     text CHECK (length(decline_reason) <= 2000),
    -- Exactly what was accepted, frozen.
    snapshot           jsonb,
    version            bigint NOT NULL DEFAULT 1,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    created_by         uuid REFERENCES users (id),
    updated_by         uuid REFERENCES users (id),
    deleted_at         timestamptz,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, number, revision),
    CHECK ((status = 'accepted') = (snapshot IS NOT NULL)),
    FOREIGN KEY (workspace_id, booking_id) REFERENCES bookings (workspace_id, id),
    FOREIGN KEY (workspace_id, supersedes_id) REFERENCES quotes (workspace_id, id)
);
CREATE INDEX quotes_booking ON quotes (workspace_id, booking_id) WHERE deleted_at IS NULL;
CREATE INDEX quotes_valid_until ON quotes (valid_until) WHERE status IN ('sent', 'viewed') AND deleted_at IS NULL;
CREATE TRIGGER quotes_updated_at BEFORE UPDATE ON quotes FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER quotes_version BEFORE UPDATE ON quotes FOR EACH ROW EXECUTE FUNCTION bump_version();

-- An accepted quote never changes; a new revision replaces it instead.
CREATE FUNCTION quotes_freeze_accepted() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF OLD.status = 'accepted' AND (
        NEW.status IS DISTINCT FROM OLD.status OR NEW.snapshot IS DISTINCT FROM OLD.snapshot
        OR NEW.line_items IS DISTINCT FROM OLD.line_items OR NEW.total_minor IS DISTINCT FROM OLD.total_minor
        OR NEW.currency IS DISTINCT FROM OLD.currency OR NEW.terms IS DISTINCT FROM OLD.terms
        OR NEW.booking_id IS DISTINCT FROM OLD.booking_id OR NEW.deleted_at IS DISTINCT FROM OLD.deleted_at
    ) THEN
        RAISE EXCEPTION 'an accepted quote cannot change' USING ERRCODE = 'check_violation',
            CONSTRAINT = 'quotes_accepted_frozen';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER quotes_freeze_accepted BEFORE UPDATE ON quotes FOR EACH ROW EXECUTE FUNCTION quotes_freeze_accepted();

-- Tasks -----------------------------------------------------------------------------

CREATE TABLE tasks (
    id             uuid PRIMARY KEY,
    workspace_id   uuid NOT NULL REFERENCES workspaces (id),
    title          text NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 300),
    notes          text CHECK (length(notes) <= 5000),
    due_at         timestamptz,
    assignee_id    uuid REFERENCES users (id),
    -- The record it is about, if any.
    related_entity text CHECK (related_entity IN ('talent', 'organisation', 'contact', 'enquiry', 'booking', 'quote')),
    related_id     uuid,
    status         text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'done')),
    done_at        timestamptz,
    done_by        uuid REFERENCES users (id),
    origin         text NOT NULL DEFAULT 'manual' CHECK (origin IN ('manual', 'rule')),
    rule_id        uuid,
    version        bigint NOT NULL DEFAULT 1,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    created_by     uuid REFERENCES users (id),
    updated_by     uuid REFERENCES users (id),
    deleted_at     timestamptz,
    CHECK ((related_entity IS NULL) = (related_id IS NULL)),
    CHECK ((status = 'done') = (done_at IS NOT NULL))
);
CREATE INDEX tasks_open ON tasks (workspace_id, due_at NULLS LAST) WHERE status = 'open' AND deleted_at IS NULL;
CREATE INDEX tasks_assignee ON tasks (workspace_id, assignee_id, status) WHERE deleted_at IS NULL;
CREATE INDEX tasks_related ON tasks (workspace_id, related_id) WHERE deleted_at IS NULL;
CREATE TRIGGER tasks_updated_at BEFORE UPDATE ON tasks FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER tasks_version BEFORE UPDATE ON tasks FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Calendar feeds ----------------------------------------------------------------------

-- A secret iCal link to the workspace's bookings, or one talent's. It works
-- only while the person who made it is still a member.
CREATE TABLE calendar_feeds (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    talent_id    uuid,
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    token_hash   bytea NOT NULL UNIQUE,
    created_by   uuid NOT NULL REFERENCES users (id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    revoked_at   timestamptz,
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id)
);
CREATE INDEX calendar_feeds_workspace ON calendar_feeds (workspace_id) WHERE revoked_at IS NULL;

-- The feed behind a link, for the public feed URL, which has no session.
CREATE FUNCTION calendar_feed_find(p_token_hash bytea)
    RETURNS TABLE (id uuid, workspace_id uuid, talent_id uuid, created_by uuid, name text)
    LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        UPDATE calendar_feeds f SET last_used_at = now()
        WHERE f.token_hash = p_token_hash AND f.revoked_at IS NULL
          AND EXISTS (SELECT 1 FROM memberships m
                      WHERE m.workspace_id = f.workspace_id AND m.user_id = f.created_by
                        AND m.deleted_at IS NULL)
        RETURNING f.id, f.workspace_id, f.talent_id, f.created_by, f.name
    $$;

-- The quote behind a buyer's link: which workspace to open, and which quote.
CREATE FUNCTION quote_public_find(p_token_hash bytea)
    RETURNS TABLE (id uuid, workspace_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT q.id, q.workspace_id FROM quotes q
        WHERE q.token_hash = p_token_hash AND q.deleted_at IS NULL
    $$;

-- Row-level security ------------------------------------------------------------------

ALTER TABLE workspace_counters ENABLE ROW LEVEL SECURITY;
ALTER TABLE enquiries ENABLE ROW LEVEL SECURITY;
ALTER TABLE enquiry_talent ENABLE ROW LEVEL SECURITY;
ALTER TABLE bookings ENABLE ROW LEVEL SECURITY;
ALTER TABLE booking_talent ENABLE ROW LEVEL SECURITY;
ALTER TABLE availability ENABLE ROW LEVEL SECURITY;
ALTER TABLE quotes ENABLE ROW LEVEL SECURITY;
ALTER TABLE tasks ENABLE ROW LEVEL SECURITY;
ALTER TABLE calendar_feeds ENABLE ROW LEVEL SECURITY;

CREATE POLICY workspace_counters_tenant ON workspace_counters FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY enquiries_tenant ON enquiries FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY enquiry_talent_tenant ON enquiry_talent FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY bookings_tenant ON bookings FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY booking_talent_tenant ON booking_talent FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY availability_tenant ON availability FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY quotes_tenant ON quotes FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY tasks_tenant ON tasks FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY calendar_feeds_tenant ON calendar_feeds FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());

-- The worker's sweep finds what is due across workspaces (ids and times
-- only); the changes themselves run inside each workspace as the app role.
CREATE POLICY bookings_worker ON bookings FOR SELECT TO onhold_worker
    USING (status IN ('hold', 'confirmed') AND deleted_at IS NULL);
CREATE POLICY quotes_worker ON quotes FOR SELECT TO onhold_worker
    USING (status IN ('sent', 'viewed') AND deleted_at IS NULL);

-- Grants ------------------------------------------------------------------------------

GRANT SELECT, INSERT, UPDATE ON workspace_counters, enquiries, bookings, availability, quotes, tasks,
    calendar_feeds TO onhold_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON enquiry_talent, booking_talent TO onhold_app;
GRANT EXECUTE ON FUNCTION booking_is_firm(bookings), calendar_feed_find(bytea), quote_public_find(bytea)
    TO onhold_app;
GRANT SELECT (id, workspace_id, status, hold_expires_at, ends_at, deleted_at) ON bookings TO onhold_worker;
GRANT SELECT (id, workspace_id, status, valid_until, deleted_at) ON quotes TO onhold_worker;
