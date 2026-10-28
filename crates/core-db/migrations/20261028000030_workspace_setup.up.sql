-- The setup questionnaire. A workspace keeps what its people ticked
-- ("what will you use OnHold for?") and what that set up: the presets it
-- combines, the modules and extras switched on, and its own words. All of it
-- can be changed again at any time; switching something off only hides it.

ALTER TABLE workspaces
    ADD COLUMN presets    text[] NOT NULL DEFAULT '{}',
    -- Finer switches inside modules: team, commission, advanced.
    ADD COLUMN extras     text[] NOT NULL DEFAULT '{}',
    -- Words per module: {"crm": {"talent": {"one": "DJ", "many": "DJs"}}}.
    ADD COLUMN vocabulary jsonb NOT NULL DEFAULT '{}'
        CHECK (jsonb_typeof(vocabulary) = 'object' AND octet_length(vocabulary::text) <= 8192),
    -- The answers, without comments; NULL until the form is first sent.
    ADD COLUMN setup      jsonb
        CHECK (setup IS NULL OR (jsonb_typeof(setup) = 'object' AND octet_length(setup::text) <= 8192)),
    ADD COLUMN setup_at   timestamptz;
GRANT UPDATE (presets, extras, vocabulary, setup, setup_at) ON workspaces TO onhold_app;

-- Workspaces made before this had every module and extra: keep them so.
UPDATE workspaces SET enabled_modules = array_append(enabled_modules, 'finance')
    WHERE NOT ('finance' = ANY (enabled_modules));
UPDATE workspaces SET extras = '{team,commission,advanced}';

-- A request or question left in the form's comments box, for OnHold's team.
CREATE TABLE setup_requests (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    body         text NOT NULL CHECK (length(btrim(body)) BETWEEN 1 AND 4000),
    created_by   uuid REFERENCES users (id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    -- When OnHold's team marked it dealt with.
    resolved_at  timestamptz,
    resolved_by  uuid REFERENCES users (id)
);
CREATE INDEX setup_requests_workspace ON setup_requests (workspace_id, created_at DESC);
CREATE INDEX setup_requests_open ON setup_requests (created_at) WHERE resolved_at IS NULL;

ALTER TABLE setup_requests ENABLE ROW LEVEL SECURITY;
CREATE POLICY setup_requests_tenant ON setup_requests FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
GRANT SELECT, INSERT ON setup_requests TO onhold_app;

-- Settings for the whole platform, such as the price list the form shows
-- an estimate from. Anyone signed in may read them; only OnHold's team
-- changes them, through the function below.
CREATE TABLE platform_settings (
    key        text PRIMARY KEY CHECK (key ~ '^[a-z_]{1,40}$'),
    value      jsonb NOT NULL CHECK (octet_length(value::text) <= 16384),
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by uuid REFERENCES users (id)
);
ALTER TABLE platform_settings ENABLE ROW LEVEL SECURITY;
CREATE POLICY platform_settings_read ON platform_settings FOR SELECT TO onhold_app USING (true);
GRANT SELECT ON platform_settings TO onhold_app;

CREATE FUNCTION admin_set_platform_setting(p_key text, p_value jsonb)
    RETURNS void
    LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM users u
                   WHERE u.id = app_user_id() AND u.is_superadmin AND u.deleted_at IS NULL) THEN
        RAISE EXCEPTION 'superadmin required' USING ERRCODE = 'insufficient_privilege';
    END IF;
    INSERT INTO platform_settings (key, value, updated_by) VALUES (p_key, p_value, app_user_id())
        ON CONFLICT (key) DO UPDATE
        SET value = EXCLUDED.value, updated_at = now(), updated_by = EXCLUDED.updated_by;
END
$$;

-- Every client's setup and comments, for OnHold's team.
DROP FUNCTION admin_list_workspaces();
CREATE FUNCTION admin_list_workspaces()
    RETURNS TABLE (id uuid, slug text, name text, plan text, status workspace_status,
                   created_at timestamptz, member_count bigint, presets text[],
                   enabled_modules text[], extras text[], setup jsonb, setup_at timestamptz,
                   open_requests bigint)
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
                WHERE m.workspace_id = w.id AND m.deleted_at IS NULL),
               w.presets, w.enabled_modules, w.extras, w.setup, w.setup_at,
               (SELECT count(*) FROM setup_requests r
                WHERE r.workspace_id = w.id AND r.resolved_at IS NULL)
        FROM workspaces w
        ORDER BY w.created_at DESC;
END
$$;

CREATE FUNCTION admin_setup_requests(p_limit integer)
    RETURNS TABLE (id uuid, workspace_id uuid, workspace_name text, body text,
                   author_name text, author_email text, created_at timestamptz,
                   resolved_at timestamptz)
    LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM users u
                   WHERE u.id = app_user_id() AND u.is_superadmin AND u.deleted_at IS NULL) THEN
        RAISE EXCEPTION 'superadmin required' USING ERRCODE = 'insufficient_privilege';
    END IF;
    -- Open ones first, then the latest dealt with.
    RETURN QUERY
        SELECT r.id, r.workspace_id, w.name, r.body, u.name, u.email::text, r.created_at, r.resolved_at
        FROM setup_requests r
        JOIN workspaces w ON w.id = r.workspace_id
        LEFT JOIN users u ON u.id = r.created_by
        ORDER BY r.resolved_at IS NOT NULL, r.created_at DESC
        LIMIT p_limit;
END
$$;

CREATE FUNCTION admin_resolve_setup_request(p_id uuid)
    RETURNS boolean
    LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM users u
                   WHERE u.id = app_user_id() AND u.is_superadmin AND u.deleted_at IS NULL) THEN
        RAISE EXCEPTION 'superadmin required' USING ERRCODE = 'insufficient_privilege';
    END IF;
    UPDATE setup_requests SET resolved_at = now(), resolved_by = app_user_id()
        WHERE id = p_id AND resolved_at IS NULL;
    RETURN FOUND;
END
$$;

REVOKE ALL ON FUNCTION admin_list_workspaces(), admin_setup_requests(integer),
    admin_resolve_setup_request(uuid), admin_set_platform_setting(text, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION admin_list_workspaces(), admin_setup_requests(integer),
    admin_resolve_setup_request(uuid), admin_set_platform_setting(text, jsonb) TO onhold_app;
