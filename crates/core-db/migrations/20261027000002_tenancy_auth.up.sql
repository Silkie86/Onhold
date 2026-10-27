-- M1: tenancy, auth, roles and permissions, audit log, event outbox.
--
-- Isolation model
-- ---------------
-- The API runs every query as the `onhold_app` role (the pool issues
-- `SET ROLE onhold_app` on connect). That role neither owns these tables nor
-- has BYPASSRLS, so row-level security always applies to it. Each transaction
-- sets two settings with `set_config(..., true)` (transaction-local):
--
--   app.workspace_id  the workspace the request acts in
--   app.user_id       the signed-in user, if any
--
-- With neither set, tenant tables return nothing. Lookups that must happen
-- before either is known (a session by token hash, a user by email at
-- sign-in, an API key by id, an invitation or magic link by token hash, an
-- OIDC identity) go through the narrow SECURITY DEFINER functions at the end
-- of this file, each returning one row for one exact key.

-- Roles ----------------------------------------------------------------------

-- Roles are cluster-wide, so creation is idempotent and the down migration
-- revokes rather than drops (other databases on the cluster may use it).
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'onhold_app') THEN
        CREATE ROLE onhold_app NOLOGIN NOBYPASSRLS;
    END IF;
END
$$;

-- The login role the API connects with must be able to SET ROLE onhold_app.
-- Superusers can already; a dedicated non-superuser owner needs membership.
DO $$
BEGIN
    IF NOT (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) THEN
        EXECUTE format('GRANT onhold_app TO %I', current_user);
    END IF;
END
$$;

GRANT USAGE ON SCHEMA public TO onhold_app;

-- Context helpers ------------------------------------------------------------

CREATE FUNCTION app_workspace_id() RETURNS uuid
    LANGUAGE sql STABLE PARALLEL SAFE
    AS $$ SELECT nullif(current_setting('app.workspace_id', true), '')::uuid $$;

CREATE FUNCTION app_user_id() RETURNS uuid
    LANGUAGE sql STABLE PARALLEL SAFE
    AS $$ SELECT nullif(current_setting('app.user_id', true), '')::uuid $$;

CREATE FUNCTION set_updated_at() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.updated_at := now();
    RETURN NEW;
END
$$;

-- Every mutable business row carries a version that increases on each
-- update, so clients (including offline ones syncing later) can send
-- If-Match and get a conflict instead of silently overwriting.
CREATE FUNCTION bump_version() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.version := OLD.version + 1;
    RETURN NEW;
END
$$;

GRANT EXECUTE ON FUNCTION app_workspace_id(), app_user_id() TO onhold_app;

-- Types ----------------------------------------------------------------------

CREATE TYPE workspace_status AS ENUM ('trial', 'active', 'suspended', 'closed');
CREATE TYPE workspace_perspective AS ENUM ('sells', 'buys', 'both');
CREATE TYPE member_role AS ENUM ('owner', 'admin', 'agent', 'finance', 'viewer', 'talent');

-- Users ----------------------------------------------------------------------

CREATE TABLE users (
    id                      uuid PRIMARY KEY,
    email                   citext NOT NULL UNIQUE CHECK (length(email) BETWEEN 3 AND 320 AND email LIKE '%_@_%'),
    name                    text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    password_hash           text,
    password_changed_at     timestamptz,
    email_verified_at       timestamptz,
    is_superadmin           boolean NOT NULL DEFAULT false,
    totp_secret_enc         bytea,
    totp_pending_secret_enc bytea,
    totp_enabled_at         timestamptz,
    totp_last_step          bigint,
    failed_login_count      integer NOT NULL DEFAULT 0,
    locked_until            timestamptz,
    created_at              timestamptz NOT NULL DEFAULT now(),
    updated_at              timestamptz NOT NULL DEFAULT now(),
    deleted_at              timestamptz,
    CHECK (totp_enabled_at IS NULL OR totp_secret_enc IS NOT NULL)
);
CREATE TRIGGER users_updated_at BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE user_recovery_codes (
    id         uuid PRIMARY KEY,
    user_id    uuid NOT NULL REFERENCES users (id),
    code_hash  bytea NOT NULL,
    used_at    timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX user_recovery_codes_user ON user_recovery_codes (user_id) WHERE used_at IS NULL;

-- Workspaces -----------------------------------------------------------------

CREATE TABLE workspaces (
    id                uuid PRIMARY KEY,
    slug              text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$'),
    name              text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    plan              text NOT NULL DEFAULT 'standard' CHECK (plan ~ '^[a-z0-9_-]{1,40}$'),
    status            workspace_status NOT NULL DEFAULT 'trial',
    perspective       workspace_perspective NOT NULL DEFAULT 'sells',
    country           char(2) NOT NULL CHECK (country ~ '^[A-Z]{2}$'),
    default_currency  char(3) NOT NULL CHECK (default_currency ~ '^[A-Z]{3}$'),
    default_timezone  text NOT NULL CHECK (length(default_timezone) BETWEEN 1 AND 64),
    locale            text NOT NULL CHECK (locale ~ '^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$'),
    enabled_modules   text[] NOT NULL DEFAULT '{}',
    branding          jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(branding) = 'object'),
    billing_contact   jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(billing_contact) = 'object'),
    plugin_slug       text CHECK (plugin_slug ~ '^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$'),
    require_staff_2fa boolean NOT NULL DEFAULT false,
    version           bigint NOT NULL DEFAULT 1,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    created_by        uuid REFERENCES users (id),
    updated_by        uuid REFERENCES users (id)
);
CREATE TRIGGER workspaces_updated_at BEFORE UPDATE ON workspaces FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER workspaces_version BEFORE UPDATE ON workspaces FOR EACH ROW EXECUTE FUNCTION bump_version();

CREATE TABLE memberships (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    user_id      uuid NOT NULL REFERENCES users (id),
    role         member_role NOT NULL,
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    deleted_at   timestamptz,
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id)
);
CREATE UNIQUE INDEX memberships_active_unique ON memberships (workspace_id, user_id) WHERE deleted_at IS NULL;
CREATE INDEX memberships_user ON memberships (user_id) WHERE deleted_at IS NULL;
CREATE TRIGGER memberships_updated_at BEFORE UPDATE ON memberships FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER memberships_version BEFORE UPDATE ON memberships FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Seeded per workspace at creation; editable later when custom roles arrive.
CREATE TABLE role_permissions (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    role         member_role NOT NULL,
    permission   text NOT NULL CHECK (permission ~ '^[a-z_]+:[a-z_]+$'),
    PRIMARY KEY (workspace_id, role, permission)
);

CREATE TABLE invitations (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    email        citext NOT NULL CHECK (length(email) BETWEEN 3 AND 320 AND email LIKE '%_@_%'),
    role         member_role NOT NULL,
    token_hash   bytea NOT NULL UNIQUE,
    expires_at   timestamptz NOT NULL,
    invited_by   uuid REFERENCES users (id),
    accepted_at  timestamptz,
    accepted_by  uuid REFERENCES users (id),
    revoked_at   timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX invitations_workspace ON invitations (workspace_id, created_at DESC);

CREATE TABLE api_keys (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    secret_hash  bytea NOT NULL,
    permissions  text[] NOT NULL,
    created_by   uuid REFERENCES users (id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz,
    last_used_at timestamptz,
    revoked_at   timestamptz
);
CREATE INDEX api_keys_workspace ON api_keys (workspace_id, created_at DESC);

-- Sign-in --------------------------------------------------------------------

CREATE TABLE sessions (
    id                         uuid PRIMARY KEY,
    user_id                    uuid NOT NULL REFERENCES users (id),
    token_hash                 bytea NOT NULL UNIQUE,
    csrf_token                 text NOT NULL,
    auth_method                text NOT NULL,
    mfa_verified               boolean NOT NULL,
    mfa_failed_attempts        integer NOT NULL DEFAULT 0,
    ip                         inet,
    user_agent                 text,
    created_at                 timestamptz NOT NULL DEFAULT now(),
    last_seen_at               timestamptz NOT NULL DEFAULT now(),
    expires_at                 timestamptz NOT NULL,
    revoked_at                 timestamptz,
    impersonated_workspace_id  uuid REFERENCES workspaces (id),
    impersonation_expires_at   timestamptz,
    CHECK ((impersonated_workspace_id IS NULL) = (impersonation_expires_at IS NULL))
);
CREATE INDEX sessions_user ON sessions (user_id) WHERE revoked_at IS NULL;

CREATE TABLE magic_links (
    id         uuid PRIMARY KEY,
    user_id    uuid NOT NULL REFERENCES users (id),
    token_hash bytea NOT NULL UNIQUE,
    expires_at timestamptz NOT NULL,
    used_at    timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE oidc_identities (
    id           uuid PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES users (id),
    provider     text NOT NULL,
    subject      text NOT NULL,
    email        citext,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    UNIQUE (provider, subject)
);
CREATE INDEX oidc_identities_user ON oidc_identities (user_id);

-- Audit and events -----------------------------------------------------------

-- Append-only: onhold_app may insert and read, never update or delete.
CREATE TABLE audit_log (
    id               uuid PRIMARY KEY,
    workspace_id     uuid NOT NULL REFERENCES workspaces (id),
    actor_user_id    uuid REFERENCES users (id),
    actor_api_key_id uuid REFERENCES api_keys (id),
    impersonator_id  uuid REFERENCES users (id),
    action           text NOT NULL,
    resource         text NOT NULL,
    object_id        uuid,
    before           jsonb,
    after            jsonb,
    ip               inet,
    user_agent       text,
    request_id       text,
    at               timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_workspace_id ON audit_log (workspace_id, id DESC);

-- The event bus outbox. Written in the same transaction as the change it
-- describes; drained by the worker from M2. workspace_id is NULL for
-- user-level events (for example a magic link request), which onhold_app may
-- write but never read back.
CREATE TABLE outbox (
    id           uuid PRIMARY KEY,
    workspace_id uuid REFERENCES workspaces (id),
    topic        text NOT NULL CHECK (topic ~ '^[a-z_]+(\.[a-z_]+)+$'),
    payload      jsonb NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz
);
CREATE INDEX outbox_unprocessed ON outbox (created_at) WHERE processed_at IS NULL;

-- Idempotency ----------------------------------------------------------------

-- Stored responses for writes sent with an Idempotency-Key header, so a
-- client that retries (a flaky connection, an offline queue replaying) gets
-- the original result instead of a second write. Scoped by a hash of the
-- caller's credential, so keys never collide across users or API keys.
-- onhold_app has no direct access: only the idem_* functions below.
CREATE TABLE idempotency_keys (
    scope_hash   bytea NOT NULL,
    key          text NOT NULL CHECK (length(key) BETWEEN 1 AND 255),
    request_hash bytea NOT NULL,
    status       smallint,
    content_type text,
    body         bytea,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scope_hash, key)
);
CREATE INDEX idempotency_keys_created ON idempotency_keys (created_at);

-- Grants ---------------------------------------------------------------------

-- No DELETE anywhere: business rows are soft-deleted or revoked.
GRANT SELECT, INSERT ON users TO onhold_app;
-- Column list deliberately omits is_superadmin and email: the API can never
-- grant operator rights, and email changes need their own verified flow.
GRANT UPDATE (name, password_hash, password_changed_at, email_verified_at,
              totp_secret_enc, totp_pending_secret_enc, totp_enabled_at, totp_last_step,
              failed_login_count, locked_until, updated_at, deleted_at) ON users TO onhold_app;
GRANT SELECT, INSERT, UPDATE ON user_recovery_codes TO onhold_app;
GRANT SELECT, INSERT ON workspaces TO onhold_app;
GRANT UPDATE (name, status, perspective, country, default_currency, default_timezone, locale,
              enabled_modules, branding, billing_contact, require_staff_2fa,
              updated_at, updated_by) ON workspaces TO onhold_app;
GRANT SELECT, INSERT, UPDATE ON memberships TO onhold_app;
GRANT SELECT, INSERT ON role_permissions TO onhold_app;
GRANT SELECT, INSERT, UPDATE ON invitations TO onhold_app;
GRANT SELECT, INSERT, UPDATE ON api_keys TO onhold_app;
GRANT SELECT, INSERT, UPDATE ON sessions TO onhold_app;
GRANT SELECT, INSERT, UPDATE ON magic_links TO onhold_app;
GRANT SELECT, INSERT, UPDATE ON oidc_identities TO onhold_app;
GRANT SELECT, INSERT ON audit_log TO onhold_app;
GRANT INSERT ON outbox TO onhold_app;

-- Row-level security ---------------------------------------------------------

ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE user_recovery_codes ENABLE ROW LEVEL SECURITY;
ALTER TABLE workspaces ENABLE ROW LEVEL SECURITY;
ALTER TABLE memberships ENABLE ROW LEVEL SECURITY;
ALTER TABLE role_permissions ENABLE ROW LEVEL SECURITY;
ALTER TABLE invitations ENABLE ROW LEVEL SECURITY;
ALTER TABLE api_keys ENABLE ROW LEVEL SECURITY;
ALTER TABLE sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE magic_links ENABLE ROW LEVEL SECURITY;
ALTER TABLE oidc_identities ENABLE ROW LEVEL SECURITY;
ALTER TABLE audit_log ENABLE ROW LEVEL SECURITY;
ALTER TABLE outbox ENABLE ROW LEVEL SECURITY;
ALTER TABLE idempotency_keys ENABLE ROW LEVEL SECURITY; -- no policies: functions only

-- A user sees themselves and the members of the workspace in context.
CREATE POLICY users_select ON users FOR SELECT TO onhold_app
    USING (id = app_user_id()
           OR EXISTS (SELECT 1 FROM memberships m
                      WHERE m.user_id = users.id
                        AND m.workspace_id = app_workspace_id()
                        AND m.deleted_at IS NULL));
CREATE POLICY users_insert ON users FOR INSERT TO onhold_app WITH CHECK (id = app_user_id());
CREATE POLICY users_update ON users FOR UPDATE TO onhold_app
    USING (id = app_user_id()) WITH CHECK (id = app_user_id());

CREATE POLICY user_recovery_codes_all ON user_recovery_codes FOR ALL TO onhold_app
    USING (user_id = app_user_id()) WITH CHECK (user_id = app_user_id());

-- A workspace is visible in context, and to its own members (for the switcher).
CREATE POLICY workspaces_select ON workspaces FOR SELECT TO onhold_app
    USING (id = app_workspace_id()
           OR EXISTS (SELECT 1 FROM memberships m
                      WHERE m.workspace_id = workspaces.id
                        AND m.user_id = app_user_id()
                        AND m.deleted_at IS NULL));
CREATE POLICY workspaces_insert ON workspaces FOR INSERT TO onhold_app WITH CHECK (id = app_workspace_id());
CREATE POLICY workspaces_update ON workspaces FOR UPDATE TO onhold_app
    USING (id = app_workspace_id()) WITH CHECK (id = app_workspace_id());

-- A user may read their own memberships everywhere; writes need the workspace in context.
CREATE POLICY memberships_select ON memberships FOR SELECT TO onhold_app
    USING (workspace_id = app_workspace_id() OR user_id = app_user_id());
CREATE POLICY memberships_insert ON memberships FOR INSERT TO onhold_app
    WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY memberships_update ON memberships FOR UPDATE TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());

CREATE POLICY role_permissions_tenant ON role_permissions FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY invitations_tenant ON invitations FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY api_keys_tenant ON api_keys FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY audit_log_tenant ON audit_log FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());

CREATE POLICY sessions_own ON sessions FOR ALL TO onhold_app
    USING (user_id = app_user_id()) WITH CHECK (user_id = app_user_id());
CREATE POLICY magic_links_own ON magic_links FOR ALL TO onhold_app
    USING (user_id = app_user_id()) WITH CHECK (user_id = app_user_id());
CREATE POLICY oidc_identities_own ON oidc_identities FOR ALL TO onhold_app
    USING (user_id = app_user_id()) WITH CHECK (user_id = app_user_id());

-- Insert-only for the API; the worker reads with its own role from M2.
CREATE POLICY outbox_insert ON outbox FOR INSERT TO onhold_app
    WITH CHECK (workspace_id IS NOT DISTINCT FROM app_workspace_id());

-- Pre-authentication lookups -------------------------------------------------
-- Each returns at most one row for one exact key, never a list.

CREATE FUNCTION auth_find_session(p_token_hash bytea)
    RETURNS TABLE (session_id uuid, user_id uuid, csrf_token text, mfa_verified boolean,
                   expires_at timestamptz, impersonated_workspace_id uuid,
                   impersonation_expires_at timestamptz)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT s.id, s.user_id, s.csrf_token, s.mfa_verified, s.expires_at,
               s.impersonated_workspace_id, s.impersonation_expires_at
        FROM sessions s JOIN users u ON u.id = s.user_id
        WHERE s.token_hash = p_token_hash
          AND s.revoked_at IS NULL
          AND s.expires_at > now()
          AND u.deleted_at IS NULL
    $$;

CREATE FUNCTION auth_find_user_by_email(p_email citext)
    RETURNS TABLE (user_id uuid, password_hash text, locked_until timestamptz)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT u.id, u.password_hash, u.locked_until
        FROM users u
        WHERE u.email = p_email AND u.deleted_at IS NULL
    $$;

CREATE FUNCTION auth_find_api_key(p_key_id uuid)
    RETURNS TABLE (workspace_id uuid, secret_hash bytea, permissions text[], created_by uuid)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT k.workspace_id, k.secret_hash, k.permissions, k.created_by
        FROM api_keys k
        WHERE k.id = p_key_id
          AND k.revoked_at IS NULL
          AND (k.expires_at IS NULL OR k.expires_at > now())
    $$;

CREATE FUNCTION auth_find_invitation(p_token_hash bytea)
    RETURNS TABLE (invitation_id uuid, workspace_id uuid, email citext, role member_role,
                   workspace_name text)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT i.id, i.workspace_id, i.email, i.role, w.name
        FROM invitations i JOIN workspaces w ON w.id = i.workspace_id
        WHERE i.token_hash = p_token_hash
          AND i.accepted_at IS NULL
          AND i.revoked_at IS NULL
          AND i.expires_at > now()
          AND w.status <> 'closed'
    $$;

CREATE FUNCTION auth_find_magic_link(p_token_hash bytea)
    RETURNS TABLE (magic_link_id uuid, user_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT m.id, m.user_id
        FROM magic_links m JOIN users u ON u.id = m.user_id
        WHERE m.token_hash = p_token_hash
          AND m.used_at IS NULL
          AND m.expires_at > now()
          AND u.deleted_at IS NULL
    $$;

CREATE FUNCTION auth_find_oidc_identity(p_provider text, p_subject text)
    RETURNS TABLE (user_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT o.user_id
        FROM oidc_identities o JOIN users u ON u.id = o.user_id
        WHERE o.provider = p_provider AND o.subject = p_subject AND u.deleted_at IS NULL
    $$;

-- Operator listing. Raises unless the user in context is a superadmin.
CREATE FUNCTION admin_list_workspaces()
    RETURNS TABLE (id uuid, slug text, name text, plan text, status workspace_status,
                   created_at timestamptz, member_count bigint)
    LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM users u
                   WHERE u.id = app_user_id() AND u.is_superadmin AND u.deleted_at IS NULL) THEN
        RAISE EXCEPTION 'superadmin required' USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN QUERY
        SELECT w.id, w.slug, w.name, w.plan, w.status, w.created_at,
               (SELECT count(*) FROM memberships m
                WHERE m.workspace_id = w.id AND m.deleted_at IS NULL)
        FROM workspaces w
        ORDER BY w.created_at DESC;
END
$$;

-- Idempotency ------------------------------------------------------------------

-- Claims a key. Returns the existing row if there is one (completed, or
-- still in flight when status is NULL); returns nothing if this call now
-- owns the key. Rows older than 24 hours are treated as gone.
CREATE FUNCTION idem_claim(p_scope bytea, p_key text, p_request_hash bytea)
    RETURNS TABLE (request_hash bytea, status smallint, content_type text, body bytea)
    LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
BEGIN
    DELETE FROM idempotency_keys i
    WHERE i.scope_hash = p_scope AND i.key = p_key AND i.created_at < now() - interval '24 hours';
    INSERT INTO idempotency_keys (scope_hash, key, request_hash)
    VALUES (p_scope, p_key, p_request_hash)
    ON CONFLICT ON CONSTRAINT idempotency_keys_pkey DO NOTHING;
    IF FOUND THEN
        RETURN;
    END IF;
    RETURN QUERY
        SELECT i.request_hash, i.status, i.content_type, i.body
        FROM idempotency_keys i
        WHERE i.scope_hash = p_scope AND i.key = p_key;
END
$$;

CREATE FUNCTION idem_complete(p_scope bytea, p_key text, p_status smallint, p_content_type text, p_body bytea)
    RETURNS void
    LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        UPDATE idempotency_keys SET status = p_status, content_type = p_content_type, body = p_body
        WHERE scope_hash = p_scope AND key = p_key AND status IS NULL
    $$;

-- Releases a claim whose request failed (5xx), so a retry can run.
CREATE FUNCTION idem_release(p_scope bytea, p_key text)
    RETURNS void
    LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        DELETE FROM idempotency_keys WHERE scope_hash = p_scope AND key = p_key AND status IS NULL
    $$;

REVOKE ALL ON FUNCTION idem_claim(bytea, text, bytea), idem_complete(bytea, text, smallint, text, bytea),
    idem_release(bytea, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION idem_claim(bytea, text, bytea), idem_complete(bytea, text, smallint, text, bytea),
    idem_release(bytea, text) TO onhold_app;

REVOKE ALL ON FUNCTION auth_find_session(bytea), auth_find_user_by_email(citext),
    auth_find_api_key(uuid), auth_find_invitation(bytea), auth_find_magic_link(bytea),
    auth_find_oidc_identity(text, text), admin_list_workspaces() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth_find_session(bytea), auth_find_user_by_email(citext),
    auth_find_api_key(uuid), auth_find_invitation(bytea), auth_find_magic_link(bytea),
    auth_find_oidc_identity(text, text), admin_list_workspaces() TO onhold_app;
