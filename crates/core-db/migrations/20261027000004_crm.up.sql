-- M3: the CRM. Talent (the generic bookable) with its members,
-- organisations and contacts, plus spreadsheet imports and saved list views.
--
-- Every record follows the core conventions: UUIDv7 id, workspace_id,
-- version, created/updated/deleted timestamps and authors, a `custom` JSONB
-- column governed by the custom-field engine, and free-form `tags`.
-- Deletes are soft, so bookings and invoices that point at a record later
-- keep working.
--
-- Links between records carry the workspace in the foreign key, so a record
-- can only ever point at another record in its own workspace, whatever the
-- application does.
--
-- Search: `search_text` is a generated, lower-cased summary of a record's
-- short fields, indexed with trigrams for "contains" and fuzzy matching.
-- Names have their own trigram index for duplicate checks.

-- Permissions -----------------------------------------------------------------

-- Granted to existing workspaces' roles exactly as `Role::default_permissions`
-- grants them to new ones.
INSERT INTO role_permissions (workspace_id, role, permission)
SELECT w.id, r.role::member_role, r.permission
FROM workspaces w
CROSS JOIN (VALUES
    ('owner', 'talent:read'), ('admin', 'talent:read'), ('agent', 'talent:read'),
    ('finance', 'talent:read'), ('viewer', 'talent:read'),
    ('owner', 'talent:write'), ('admin', 'talent:write'), ('agent', 'talent:write'),
    ('owner', 'organisation:read'), ('admin', 'organisation:read'), ('agent', 'organisation:read'),
    ('finance', 'organisation:read'), ('viewer', 'organisation:read'),
    ('owner', 'organisation:write'), ('admin', 'organisation:write'), ('agent', 'organisation:write'),
    ('owner', 'contact:read'), ('admin', 'contact:read'), ('agent', 'contact:read'),
    ('finance', 'contact:read'), ('viewer', 'contact:read'),
    ('owner', 'contact:write'), ('admin', 'contact:write'), ('agent', 'contact:write')
) AS r (role, permission)
ON CONFLICT DO NOTHING;

-- Helpers ---------------------------------------------------------------------

-- array_to_string is only STABLE (it formats any element type); for text[]
-- the result never changes, which generated columns need to know.
CREATE FUNCTION search_join(text[]) RETURNS text
    LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT
    AS $$ SELECT array_to_string($1, ' ') $$;

-- Talent ----------------------------------------------------------------------

CREATE TYPE talent_kind AS ENUM ('person', 'act', 'space', 'equipment');

CREATE TABLE talent (
    id                   uuid PRIMARY KEY,
    workspace_id         uuid NOT NULL REFERENCES workspaces (id),
    kind                 talent_kind NOT NULL DEFAULT 'person',
    -- The stage or display name.
    name                 text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    legal_name           text CHECK (length(legal_name) <= 200),
    -- Free text: solo, band, DJ, speaker, studio…
    talent_type          text CHECK (length(talent_type) <= 100),
    bio                  text CHECK (length(bio) <= 20000),
    base_location        text CHECK (length(base_location) <= 200),
    default_fee_minor    bigint CHECK (default_fee_minor >= 0),
    default_fee_currency char(3) CHECK (default_fee_currency ~ '^[A-Z]{3}$'),
    -- Hundredths of a percent: 1500 is 15%.
    commission_bps       integer CHECK (commission_bps BETWEEN 0 AND 10000),
    status               text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
    portal_user_id       uuid REFERENCES users (id),
    tags                 text[] NOT NULL DEFAULT '{}',
    custom               jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom) = 'object'),
    version              bigint NOT NULL DEFAULT 1,
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    created_by           uuid REFERENCES users (id),
    updated_by           uuid REFERENCES users (id),
    deleted_at           timestamptz,
    search_text          text GENERATED ALWAYS AS (lower(
        name || ' ' || coalesce(legal_name, '') || ' ' || coalesce(talent_type, '') || ' '
        || coalesce(base_location, '') || ' ' || search_join(tags))) STORED,
    UNIQUE (workspace_id, id),
    CHECK ((default_fee_minor IS NULL) = (default_fee_currency IS NULL))
);
CREATE INDEX talent_workspace ON talent (workspace_id, name) WHERE deleted_at IS NULL;
CREATE INDEX talent_search ON talent USING gin (search_text gin_trgm_ops);
CREATE INDEX talent_name_trgm ON talent USING gin (lower(name) gin_trgm_ops) WHERE deleted_at IS NULL;
CREATE INDEX talent_tags ON talent USING gin (tags);
CREATE INDEX talent_custom ON talent USING gin (custom jsonb_path_ops);
CREATE TRIGGER talent_updated_at BEFORE UPDATE ON talent FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER talent_version BEFORE UPDATE ON talent FOR EACH ROW EXECUTE FUNCTION bump_version();

-- A person in an act or crew.
CREATE TABLE talent_members (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    talent_id    uuid NOT NULL,
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    role         text CHECK (length(role) <= 200),
    email        text CHECK (length(email) <= 320),
    phone        text CHECK (length(phone) <= 50),
    -- Dietary, travel and other notes.
    notes        text CHECK (length(notes) <= 5000),
    position     integer NOT NULL DEFAULT 0,
    custom       jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom) = 'object'),
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    deleted_at   timestamptz,
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id)
);
CREATE INDEX talent_members_talent ON talent_members (talent_id, position) WHERE deleted_at IS NULL;
CREATE TRIGGER talent_members_updated_at BEFORE UPDATE ON talent_members FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER talent_members_version BEFORE UPDATE ON talent_members FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Organisations ---------------------------------------------------------------

CREATE TABLE organisations (
    id                 uuid PRIMARY KEY,
    workspace_id       uuid NOT NULL REFERENCES workspaces (id),
    name               text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    -- Free text: venue, promoter, brand, festival, agency…
    org_type           text CHECK (length(org_type) <= 100),
    -- [{label, line1, line2, city, region, postcode, country}]
    addresses          jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(addresses) = 'array'),
    tax_id             text CHECK (length(tax_id) <= 50),
    default_currency   char(3) CHECK (default_currency ~ '^[A-Z]{3}$'),
    payment_terms_days integer CHECK (payment_terms_days BETWEEN 0 AND 365),
    notes              text CHECK (length(notes) <= 20000),
    tags               text[] NOT NULL DEFAULT '{}',
    custom             jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom) = 'object'),
    version            bigint NOT NULL DEFAULT 1,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    created_by         uuid REFERENCES users (id),
    updated_by         uuid REFERENCES users (id),
    deleted_at         timestamptz,
    search_text        text GENERATED ALWAYS AS (lower(
        name || ' ' || coalesce(org_type, '') || ' ' || coalesce(tax_id, '') || ' '
        || search_join(tags))) STORED,
    UNIQUE (workspace_id, id)
);
CREATE INDEX organisations_workspace ON organisations (workspace_id, name) WHERE deleted_at IS NULL;
CREATE INDEX organisations_search ON organisations USING gin (search_text gin_trgm_ops);
CREATE INDEX organisations_name_trgm ON organisations USING gin (lower(name) gin_trgm_ops) WHERE deleted_at IS NULL;
CREATE INDEX organisations_tags ON organisations USING gin (tags);
CREATE INDEX organisations_custom ON organisations USING gin (custom jsonb_path_ops);
CREATE TRIGGER organisations_updated_at BEFORE UPDATE ON organisations FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER organisations_version BEFORE UPDATE ON organisations FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Contacts --------------------------------------------------------------------

CREATE TABLE contacts (
    id                uuid PRIMARY KEY,
    workspace_id      uuid NOT NULL REFERENCES workspaces (id),
    organisation_id   uuid,
    name              text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    -- Lower-cased; the first is the primary address.
    emails            text[] NOT NULL DEFAULT '{}' CHECK (cardinality(emails) <= 10),
    -- As entered; the first is the primary number.
    phones            text[] NOT NULL DEFAULT '{}' CHECK (cardinality(phones) <= 10),
    -- The last nine digits of each phone number, for duplicate checks.
    phone_keys        text[] NOT NULL DEFAULT '{}',
    -- Job title or role at the organisation.
    role              text CHECK (length(role) <= 200),
    preferred_channel text CHECK (preferred_channel IN ('email', 'phone', 'sms', 'whatsapp')),
    -- Consent to marketing on each channel.
    email_opt_in      boolean NOT NULL DEFAULT false,
    sms_opt_in        boolean NOT NULL DEFAULT false,
    whatsapp_opt_in   boolean NOT NULL DEFAULT false,
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
        name || ' ' || coalesce(role, '') || ' ' || search_join(emails) || ' '
        || search_join(phones) || ' ' || search_join(tags))) STORED,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, organisation_id) REFERENCES organisations (workspace_id, id)
);
CREATE INDEX contacts_workspace ON contacts (workspace_id, name) WHERE deleted_at IS NULL;
CREATE INDEX contacts_organisation ON contacts (organisation_id) WHERE deleted_at IS NULL;
CREATE INDEX contacts_search ON contacts USING gin (search_text gin_trgm_ops);
CREATE INDEX contacts_name_trgm ON contacts USING gin (lower(name) gin_trgm_ops) WHERE deleted_at IS NULL;
CREATE INDEX contacts_emails ON contacts USING gin (emails);
CREATE INDEX contacts_phone_keys ON contacts USING gin (phone_keys);
CREATE INDEX contacts_tags ON contacts USING gin (tags);
CREATE INDEX contacts_custom ON contacts USING gin (custom jsonb_path_ops);
CREATE TRIGGER contacts_updated_at BEFORE UPDATE ON contacts FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER contacts_version BEFORE UPDATE ON contacts FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Imports ---------------------------------------------------------------------

-- A spreadsheet being imported: uploaded, mapped, checked with a dry run,
-- then run by the worker. The file itself is dropped once the run ends.
CREATE TABLE imports (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces (id),
    entity        text NOT NULL CHECK (entity IN ('talent', 'organisation', 'contact')),
    filename      text CHECK (length(filename) <= 255),
    status        text NOT NULL DEFAULT 'uploaded'
                  CHECK (status IN ('uploaded', 'queued', 'running', 'done', 'failed')),
    csv           text,
    headers       text[] NOT NULL,
    row_count     integer NOT NULL,
    mapping       jsonb,
    options       jsonb,
    created_count integer NOT NULL DEFAULT 0,
    updated_count integer NOT NULL DEFAULT 0,
    skipped_count integer NOT NULL DEFAULT 0,
    failed_count  integer NOT NULL DEFAULT 0,
    -- How many rows the worker has dealt with, for progress.
    processed     integer NOT NULL DEFAULT 0,
    -- [{row, column, message}], capped.
    issues        jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(issues) = 'array'),
    error         text,
    version       bigint NOT NULL DEFAULT 1,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    created_by    uuid REFERENCES users (id),
    started_at    timestamptz,
    finished_at   timestamptz
);
CREATE INDEX imports_workspace ON imports (workspace_id, created_at DESC);
CREATE TRIGGER imports_updated_at BEFORE UPDATE ON imports FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER imports_version BEFORE UPDATE ON imports FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Saved list views ------------------------------------------------------------

-- Columns, sort and filters for a list. A view with no owner is shared with
-- the whole workspace; otherwise only its owner sees it.
CREATE TABLE saved_views (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    entity       text NOT NULL CHECK (entity ~ '^[a-z_]{1,40}$'),
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    owner_id     uuid REFERENCES users (id),
    config       jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(config) = 'object'),
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    deleted_at   timestamptz
);
CREATE INDEX saved_views_workspace ON saved_views (workspace_id, entity) WHERE deleted_at IS NULL;
CREATE TRIGGER saved_views_updated_at BEFORE UPDATE ON saved_views FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER saved_views_version BEFORE UPDATE ON saved_views FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Activity timeline -------------------------------------------------------------

-- A record's history is its audit entries.
CREATE INDEX audit_log_object ON audit_log (workspace_id, object_id, at DESC) WHERE object_id IS NOT NULL;
-- Emails sent to a contact's addresses.
CREATE INDEX outbound_messages_to ON outbound_messages (workspace_id, lower(to_address), created_at DESC);

-- Row-level security ------------------------------------------------------------

ALTER TABLE talent ENABLE ROW LEVEL SECURITY;
ALTER TABLE talent_members ENABLE ROW LEVEL SECURITY;
ALTER TABLE organisations ENABLE ROW LEVEL SECURITY;
ALTER TABLE contacts ENABLE ROW LEVEL SECURITY;
ALTER TABLE imports ENABLE ROW LEVEL SECURITY;
ALTER TABLE saved_views ENABLE ROW LEVEL SECURITY;

CREATE POLICY talent_tenant ON talent FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY talent_members_tenant ON talent_members FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY organisations_tenant ON organisations FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY contacts_tenant ON contacts FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY imports_tenant ON imports FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY saved_views_tenant ON saved_views FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id() AND (owner_id IS NULL OR owner_id = app_user_id()))
    WITH CHECK (workspace_id = app_workspace_id() AND (owner_id IS NULL OR owner_id = app_user_id()));

-- The worker deletes uploads that were never run.
CREATE POLICY imports_worker ON imports FOR SELECT TO onhold_worker USING (status = 'uploaded');
CREATE POLICY imports_worker_delete ON imports FOR DELETE TO onhold_worker USING (status = 'uploaded');

-- Grants ------------------------------------------------------------------------

GRANT SELECT, INSERT, UPDATE ON talent, talent_members, organisations, contacts, imports, saved_views
    TO onhold_app;
GRANT EXECUTE ON FUNCTION search_join(text[]) TO onhold_app;
GRANT SELECT (id, status, created_at), DELETE ON imports TO onhold_worker;
