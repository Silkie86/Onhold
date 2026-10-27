-- M2: generic engines. Custom fields, pipelines, templates, automation
-- rules, the job queue, webhooks and outbound messages, plus `things`, a
-- placeholder entity that exercises every engine until real modules arrive.
--
-- Queue model
-- -----------
-- The worker drains `outbox` and runs `jobs` across every workspace, so it
-- connects as a second role, `onhold_worker`. That role is also subject to
-- row-level security, and its policies cover only the queue tables (outbox,
-- jobs, job_runs, the rule schedule columns and system mail). To touch a
-- workspace's data a job switches to the API's `onhold_app` role with that
-- workspace in context, exactly like a request does.
--
-- The drain gives each event a gapless, commit-ordered `seq` while holding
-- an advisory lock, so `seq > cursor` never skips an event that committed
-- late. That makes the outbox safe to page through as the workspace's event
-- stream (and the changes feed for offline clients).

-- Roles ----------------------------------------------------------------------

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'onhold_worker') THEN
        CREATE ROLE onhold_worker NOLOGIN NOBYPASSRLS;
    END IF;
END
$$;

DO $$
BEGIN
    IF NOT (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) THEN
        EXECUTE format('GRANT onhold_worker TO %I', current_user);
    END IF;
END
$$;

GRANT USAGE ON SCHEMA public TO onhold_worker;

-- Permissions added in M2, granted to existing workspaces' roles exactly as
-- `Role::default_permissions` grants them to new ones.
INSERT INTO role_permissions (workspace_id, role, permission)
SELECT w.id, r.role::member_role, r.permission
FROM workspaces w
CROSS JOIN (VALUES
    ('owner', 'config:write'), ('admin', 'config:write'),
    ('owner', 'thing:read'), ('admin', 'thing:read'), ('agent', 'thing:read'),
    ('finance', 'thing:read'), ('viewer', 'thing:read'),
    ('owner', 'thing:write'), ('admin', 'thing:write'), ('agent', 'thing:write'),
    ('owner', 'event:read'), ('admin', 'event:read'), ('agent', 'event:read'),
    ('finance', 'event:read'), ('viewer', 'event:read'),
    ('owner', 'webhook:read'), ('admin', 'webhook:read'),
    ('owner', 'webhook:write'), ('admin', 'webhook:write'),
    ('owner', 'job:read'), ('admin', 'job:read'),
    ('owner', 'job:write'), ('admin', 'job:write')
) AS r (role, permission)
ON CONFLICT DO NOTHING;

-- Workspaces and audit -------------------------------------------------------

-- The automation kill switch: when set, no rule runs in the workspace.
ALTER TABLE workspaces ADD COLUMN automations_paused boolean NOT NULL DEFAULT false;
GRANT UPDATE (automations_paused) ON workspaces TO onhold_app;

-- Changes made by an automation rule name it.
ALTER TABLE audit_log ADD COLUMN actor_rule_id uuid;

-- Events ---------------------------------------------------------------------

CREATE SEQUENCE outbox_seq AS bigint;
ALTER TABLE outbox
    ADD COLUMN seq bigint UNIQUE,
    -- Encrypted data a consumer needs once and nobody may read back, such as
    -- the link in an invitation email. Moved to the job that uses it, then gone.
    ADD COLUMN secret bytea;
CREATE INDEX outbox_workspace_seq ON outbox (workspace_id, seq) WHERE seq IS NOT NULL;

-- How far the worker has deleted old events. A client whose cursor is below
-- this may have missed events and must resync from scratch.
CREATE TABLE outbox_pruned (
    singleton   boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    through_seq bigint NOT NULL
);

-- Jobs -----------------------------------------------------------------------

CREATE TYPE job_status AS ENUM ('queued', 'running', 'succeeded', 'dead', 'cancelled');

CREATE TABLE jobs (
    id           uuid PRIMARY KEY,
    workspace_id uuid REFERENCES workspaces (id),
    kind         text NOT NULL CHECK (kind ~ '^[a-z_]+(\.[a-z_]+)+$'),
    payload      jsonb NOT NULL DEFAULT '{}',
    secret       bytea,
    -- At most one job per (workspace, kind, dedupe_key): enqueueing the same
    -- logical job twice is a no-op.
    dedupe_key   text CHECK (length(dedupe_key) <= 300),
    priority     smallint NOT NULL DEFAULT 0,
    status       job_status NOT NULL DEFAULT 'queued',
    run_at       timestamptz NOT NULL DEFAULT now(),
    attempts     integer NOT NULL DEFAULT 0,
    max_attempts integer NOT NULL DEFAULT 10 CHECK (max_attempts BETWEEN 1 AND 100),
    locked_until timestamptz,
    last_error   text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    finished_at  timestamptz
);
CREATE UNIQUE INDEX jobs_dedupe ON jobs (workspace_id, kind, dedupe_key) NULLS NOT DISTINCT
    WHERE dedupe_key IS NOT NULL;
CREATE INDEX jobs_ready ON jobs (priority DESC, run_at) WHERE status = 'queued';
CREATE INDEX jobs_leased ON jobs (locked_until) WHERE status = 'running';
CREATE INDEX jobs_workspace ON jobs (workspace_id, created_at DESC);
CREATE TRIGGER jobs_updated_at BEFORE UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- One row per attempt: the per-job log.
CREATE TABLE job_runs (
    id           uuid PRIMARY KEY,
    job_id       uuid NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    workspace_id uuid REFERENCES workspaces (id),
    attempt      integer NOT NULL,
    started_at   timestamptz NOT NULL,
    finished_at  timestamptz NOT NULL DEFAULT now(),
    succeeded    boolean NOT NULL,
    error        text,
    log          jsonb NOT NULL DEFAULT '[]'
);
CREATE INDEX job_runs_job ON job_runs (job_id, attempt);

-- Custom fields --------------------------------------------------------------

CREATE TABLE custom_field_defs (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces (id),
    entity        text NOT NULL CHECK (entity ~ '^[a-z_]{1,40}$'),
    key           text NOT NULL CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
    label         text NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 200),
    field_type    text NOT NULL CHECK (field_type IN (
        'text', 'long_text', 'number', 'money', 'date', 'datetime', 'boolean', 'select',
        'multi_select', 'url', 'email', 'phone', 'file', 'relation')),
    required      boolean NOT NULL DEFAULT false,
    default_value jsonb,
    -- Type-specific rules: regex, min, max, min_length, max_length,
    -- options ([{value, label}]), relation entity, money currency.
    validation    jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(validation) = 'object'),
    visibility    text NOT NULL DEFAULT 'staff' CHECK (visibility IN ('staff', 'portal', 'both')),
    searchable    boolean NOT NULL DEFAULT false,
    field_group   text CHECK (length(field_group) <= 100),
    position      integer NOT NULL DEFAULT 0,
    help_text     text CHECK (length(help_text) <= 1000),
    version       bigint NOT NULL DEFAULT 1,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    created_by    uuid REFERENCES users (id),
    updated_by    uuid REFERENCES users (id),
    -- Removing a field archives it: values stay in rows, the key stays taken.
    archived_at   timestamptz,
    UNIQUE (workspace_id, entity, key)
);
CREATE TRIGGER custom_field_defs_updated_at BEFORE UPDATE ON custom_field_defs FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER custom_field_defs_version BEFORE UPDATE ON custom_field_defs FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Pipelines ------------------------------------------------------------------

CREATE TABLE pipelines (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    entity       text NOT NULL CHECK (entity ~ '^[a-z_]{1,40}$'),
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    is_default   boolean NOT NULL DEFAULT false,
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    deleted_at   timestamptz
);
CREATE UNIQUE INDEX pipelines_one_default ON pipelines (workspace_id, entity)
    WHERE is_default AND deleted_at IS NULL;
CREATE TRIGGER pipelines_updated_at BEFORE UPDATE ON pipelines FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER pipelines_version BEFORE UPDATE ON pipelines FOR EACH ROW EXECUTE FUNCTION bump_version();

CREATE TABLE pipeline_stages (
    id              uuid PRIMARY KEY,
    workspace_id    uuid NOT NULL REFERENCES workspaces (id),
    pipeline_id     uuid NOT NULL REFERENCES pipelines (id),
    key             text NOT NULL CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
    name            text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    position        integer NOT NULL,
    colour          text CHECK (colour ~ '^#[0-9a-fA-F]{6}$'),
    sla_days        integer CHECK (sla_days BETWEEN 1 AND 3650),
    outcome         text NOT NULL DEFAULT 'open' CHECK (outcome IN ('open', 'won', 'lost')),
    -- Custom field keys that must have a value before an object may enter.
    required_fields text[] NOT NULL DEFAULT '{}',
    -- Stage keys this stage may move to; NULL means any.
    allowed_next    text[],
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    archived_at     timestamptz,
    UNIQUE (pipeline_id, key)
);
CREATE TRIGGER pipeline_stages_updated_at BEFORE UPDATE ON pipeline_stages FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- Things: the placeholder entity ----------------------------------------------

CREATE TABLE things (
    id               uuid PRIMARY KEY,
    workspace_id     uuid NOT NULL REFERENCES workspaces (id),
    name             text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    pipeline_id      uuid REFERENCES pipelines (id),
    stage_id         uuid REFERENCES pipeline_stages (id),
    stage_entered_at timestamptz,
    due_at           timestamptz,
    custom           jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom) = 'object'),
    version          bigint NOT NULL DEFAULT 1,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    created_by       uuid REFERENCES users (id),
    updated_by       uuid REFERENCES users (id),
    deleted_at       timestamptz
);
CREATE INDEX things_workspace ON things (workspace_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX things_due ON things (workspace_id, due_at) WHERE deleted_at IS NULL;
CREATE INDEX things_custom ON things USING gin (custom jsonb_path_ops);
CREATE TRIGGER things_updated_at BEFORE UPDATE ON things FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER things_version BEFORE UPDATE ON things FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Templates ------------------------------------------------------------------

CREATE TABLE templates (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    kind         text NOT NULL CHECK (kind IN ('email', 'sms', 'whatsapp', 'notification', 'partial')),
    -- Stable name used by rules and includes, e.g. `welcome` or `system.invitation`.
    key          text NOT NULL CHECK (key ~ '^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$' AND length(key) <= 100),
    locale       text NOT NULL DEFAULT 'en' CHECK (locale ~ '^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$'),
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    subject      text CHECK (length(subject) <= 1000),
    body         text NOT NULL CHECK (length(body) <= 200000),
    description  text CHECK (length(description) <= 2000),
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    archived_at  timestamptz
);
CREATE UNIQUE INDEX templates_key ON templates (workspace_id, key, locale) WHERE archived_at IS NULL;
CREATE TRIGGER templates_updated_at BEFORE UPDATE ON templates FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER templates_version BEFORE UPDATE ON templates FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Automation rules -----------------------------------------------------------

CREATE TABLE rules (
    id              uuid PRIMARY KEY,
    workspace_id    uuid NOT NULL REFERENCES workspaces (id),
    name            text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    enabled         boolean NOT NULL DEFAULT true,
    dry_run         boolean NOT NULL DEFAULT false,
    once_per_object boolean NOT NULL DEFAULT true,
    trigger         jsonb NOT NULL CHECK (jsonb_typeof(trigger) = 'object'),
    conditions      jsonb NOT NULL DEFAULT '{"all": []}' CHECK (jsonb_typeof(conditions) = 'object'),
    actions         jsonb NOT NULL CHECK (jsonb_typeof(actions) = 'array'),
    -- Denormalised from `trigger` for the dispatcher and the scheduler.
    trigger_kind    text NOT NULL CHECK (trigger_kind IN ('event', 'date_offset', 'schedule')),
    trigger_event   text,
    -- When the scheduler next looks at a time-based rule, and (for date
    -- offsets) up to where it has already looked.
    next_run_at     timestamptz,
    scanned_until   timestamptz,
    version         bigint NOT NULL DEFAULT 1,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    created_by      uuid REFERENCES users (id),
    updated_by      uuid REFERENCES users (id),
    deleted_at      timestamptz
);
CREATE INDEX rules_event ON rules (workspace_id, trigger_event) WHERE deleted_at IS NULL AND enabled;
CREATE INDEX rules_due ON rules (next_run_at) WHERE deleted_at IS NULL AND enabled AND next_run_at IS NOT NULL;
CREATE TRIGGER rules_updated_at BEFORE UPDATE ON rules FOR EACH ROW EXECUTE FUNCTION set_updated_at();
-- Only edits bump the version; the scheduler's bookkeeping does not.
CREATE FUNCTION rules_bump_version() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF (NEW.name, NEW.enabled, NEW.dry_run, NEW.once_per_object, NEW.trigger, NEW.conditions,
        NEW.actions, NEW.deleted_at)
       IS DISTINCT FROM
       (OLD.name, OLD.enabled, OLD.dry_run, OLD.once_per_object, OLD.trigger, OLD.conditions,
        OLD.actions, OLD.deleted_at) THEN
        NEW.version := OLD.version + 1;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER rules_version BEFORE UPDATE ON rules FOR EACH ROW EXECUTE FUNCTION rules_bump_version();

-- What each rule did and why: the execution log shown in the UI.
CREATE TABLE rule_runs (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    rule_id      uuid NOT NULL REFERENCES rules (id),
    event_id     uuid,
    object_id    uuid,
    outcome      text NOT NULL CHECK (outcome IN ('acted', 'dry_run', 'skipped', 'failed')),
    reason       text,
    actions      jsonb NOT NULL DEFAULT '[]',
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX rule_runs_rule ON rule_runs (rule_id, created_at DESC);

-- The once-per-object guard.
CREATE TABLE rule_fires (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    rule_id      uuid NOT NULL REFERENCES rules (id),
    object_id    uuid NOT NULL,
    fired_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (rule_id, object_id)
);

-- Webhooks -------------------------------------------------------------------

CREATE TABLE webhook_endpoints (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    url          text NOT NULL CHECK (url ~ '^https?://' AND length(url) <= 2000),
    description  text CHECK (length(description) <= 500),
    -- Topic patterns: `thing.created`, `thing.*` or `*`.
    events       text[] NOT NULL CHECK (cardinality(events) BETWEEN 1 AND 100),
    secret_enc   bytea NOT NULL,
    enabled      boolean NOT NULL DEFAULT true,
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    deleted_at   timestamptz
);
CREATE TRIGGER webhook_endpoints_updated_at BEFORE UPDATE ON webhook_endpoints FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER webhook_endpoints_version BEFORE UPDATE ON webhook_endpoints FOR EACH ROW EXECUTE FUNCTION bump_version();

CREATE TABLE webhook_deliveries (
    id               uuid PRIMARY KEY,
    workspace_id     uuid NOT NULL REFERENCES workspaces (id),
    endpoint_id      uuid NOT NULL REFERENCES webhook_endpoints (id),
    event_id         uuid NOT NULL,
    job_id           uuid,
    attempt          integer NOT NULL,
    succeeded        boolean NOT NULL,
    status_code      integer,
    error            text,
    response_excerpt text,
    duration_ms      integer NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX webhook_deliveries_endpoint ON webhook_deliveries (endpoint_id, created_at DESC);

-- Outbound messages ----------------------------------------------------------

-- Every message sent, as rendered, for the audit trail. Links that grant
-- access (invitations, sign-in links) are redacted before storing.
CREATE TABLE outbound_messages (
    id                  uuid PRIMARY KEY,
    -- NULL for platform mail that belongs to a user, not a workspace.
    workspace_id        uuid REFERENCES workspaces (id),
    user_id             uuid REFERENCES users (id),
    channel             text NOT NULL CHECK (channel IN ('email')),
    to_address          text NOT NULL,
    subject             text,
    body_html           text,
    body_text           text NOT NULL,
    template_key        text,
    rule_id             uuid REFERENCES rules (id),
    job_id              uuid,
    status              text NOT NULL CHECK (status IN ('sent', 'failed')),
    provider_message_id text,
    error               text,
    created_at          timestamptz NOT NULL DEFAULT now(),
    CHECK (workspace_id IS NOT NULL OR user_id IS NOT NULL)
);
CREATE INDEX outbound_messages_workspace ON outbound_messages (workspace_id, created_at DESC);

-- Row-level security ---------------------------------------------------------

ALTER TABLE jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE job_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE custom_field_defs ENABLE ROW LEVEL SECURITY;
ALTER TABLE pipelines ENABLE ROW LEVEL SECURITY;
ALTER TABLE pipeline_stages ENABLE ROW LEVEL SECURITY;
ALTER TABLE things ENABLE ROW LEVEL SECURITY;
ALTER TABLE templates ENABLE ROW LEVEL SECURITY;
ALTER TABLE rules ENABLE ROW LEVEL SECURITY;
ALTER TABLE rule_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE rule_fires ENABLE ROW LEVEL SECURITY;
ALTER TABLE webhook_endpoints ENABLE ROW LEVEL SECURITY;
ALTER TABLE webhook_deliveries ENABLE ROW LEVEL SECURITY;
ALTER TABLE outbound_messages ENABLE ROW LEVEL SECURITY;
ALTER TABLE outbox_pruned ENABLE ROW LEVEL SECURITY;

-- Not tenant data: one number every workspace's event stream checks against.
CREATE POLICY outbox_pruned_read ON outbox_pruned FOR SELECT TO onhold_app USING (true);
CREATE POLICY outbox_pruned_worker ON outbox_pruned FOR ALL TO onhold_worker USING (true) WITH CHECK (true);

-- The API reads its own workspace's published events (the event stream).
CREATE POLICY outbox_events ON outbox FOR SELECT TO onhold_app
    USING (workspace_id = app_workspace_id() AND seq IS NOT NULL);

CREATE POLICY custom_field_defs_tenant ON custom_field_defs FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY pipelines_tenant ON pipelines FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY pipeline_stages_tenant ON pipeline_stages FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY things_tenant ON things FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY templates_tenant ON templates FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY rules_tenant ON rules FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY rule_runs_tenant ON rule_runs FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY rule_fires_tenant ON rule_fires FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY webhook_endpoints_tenant ON webhook_endpoints FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY webhook_deliveries_tenant ON webhook_deliveries FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY jobs_tenant ON jobs FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY job_runs_tenant ON job_runs FOR SELECT TO onhold_app
    USING (workspace_id = app_workspace_id());
-- Platform mail (no workspace) is visible to the user it was sent to.
CREATE POLICY outbound_messages_tenant ON outbound_messages FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()
           OR (workspace_id IS NULL AND user_id = app_user_id()))
    WITH CHECK (workspace_id = app_workspace_id()
                OR (workspace_id IS NULL AND user_id = app_user_id()));

-- The worker sees the queue tables in full, and nothing else.
CREATE POLICY outbox_worker ON outbox FOR ALL TO onhold_worker USING (true) WITH CHECK (true);
CREATE POLICY jobs_worker ON jobs FOR ALL TO onhold_worker USING (true) WITH CHECK (true);
CREATE POLICY job_runs_worker ON job_runs FOR ALL TO onhold_worker USING (true) WITH CHECK (true);
CREATE POLICY rules_worker ON rules FOR SELECT TO onhold_worker USING (true);
CREATE POLICY rules_worker_schedule ON rules FOR UPDATE TO onhold_worker USING (true) WITH CHECK (true);
CREATE POLICY workspaces_worker ON workspaces FOR SELECT TO onhold_worker USING (true);
CREATE POLICY idempotency_keys_worker ON idempotency_keys FOR DELETE TO onhold_worker USING (true);

-- Grants ---------------------------------------------------------------------

-- No DELETE for the API, as before; archived and soft-deleted rows stay.
GRANT SELECT (id, workspace_id, topic, payload, created_at, seq) ON outbox TO onhold_app;
GRANT SELECT, INSERT, UPDATE ON custom_field_defs, pipelines, pipeline_stages, things, templates,
    rules, webhook_endpoints TO onhold_app;
GRANT SELECT, INSERT ON rule_runs, rule_fires, webhook_deliveries, outbound_messages TO onhold_app;
GRANT SELECT (id, workspace_id, kind, payload, dedupe_key, priority, status, run_at, attempts,
              max_attempts, locked_until, last_error, created_at, updated_at, finished_at)
    ON jobs TO onhold_app;
GRANT INSERT ON jobs TO onhold_app;
-- Retrying a dead job from the UI.
GRANT UPDATE (status, run_at, attempts, last_error, finished_at) ON jobs TO onhold_app;
GRANT SELECT ON job_runs TO onhold_app;
GRANT SELECT ON outbox_pruned TO onhold_app;

GRANT SELECT, UPDATE (seq, processed_at, secret), DELETE ON outbox TO onhold_worker;
GRANT USAGE ON SEQUENCE outbox_seq TO onhold_worker;
GRANT SELECT, INSERT, UPDATE ON outbox_pruned TO onhold_worker;
GRANT SELECT, INSERT, UPDATE, DELETE ON jobs, job_runs TO onhold_worker;
GRANT SELECT (id, workspace_id, trigger, trigger_kind, enabled, deleted_at, next_run_at, scanned_until)
    ON rules TO onhold_worker;
GRANT UPDATE (next_run_at, scanned_until) ON rules TO onhold_worker;
GRANT SELECT (id, status, default_timezone, automations_paused) ON workspaces TO onhold_worker;
GRANT SELECT (created_at), DELETE ON idempotency_keys TO onhold_worker;

-- Wake-ups --------------------------------------------------------------------

-- New events and jobs wake the worker at commit instead of waiting for its
-- next poll.
CREATE FUNCTION notify_channel() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    PERFORM pg_notify(TG_ARGV[0], '');
    RETURN NULL;
END
$$;
CREATE TRIGGER outbox_notify AFTER INSERT ON outbox
    FOR EACH STATEMENT EXECUTE FUNCTION notify_channel('onhold_outbox');
CREATE TRIGGER jobs_notify AFTER INSERT ON jobs
    FOR EACH STATEMENT EXECUTE FUNCTION notify_channel('onhold_jobs');

-- Operator view of the queue ---------------------------------------------------

CREATE FUNCTION admin_job_summary()
    RETURNS TABLE (workspace_id uuid, workspace_slug text, kind text, status job_status,
                   jobs bigint, oldest timestamptz)
    LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM users u
                   WHERE u.id = app_user_id() AND u.is_superadmin AND u.deleted_at IS NULL) THEN
        RAISE EXCEPTION 'superadmin required' USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN QUERY
        SELECT j.workspace_id, w.slug, j.kind, j.status, count(*), min(j.created_at)
        FROM jobs j LEFT JOIN workspaces w ON w.id = j.workspace_id
        WHERE j.status IN ('queued', 'running', 'dead')
        GROUP BY j.workspace_id, w.slug, j.kind, j.status
        ORDER BY j.status, count(*) DESC;
END
$$;
REVOKE ALL ON FUNCTION admin_job_summary() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION admin_job_summary() TO onhold_app;
