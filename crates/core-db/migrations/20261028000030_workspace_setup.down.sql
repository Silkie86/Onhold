DROP FUNCTION admin_list_workspaces();
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
REVOKE ALL ON FUNCTION admin_list_workspaces() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION admin_list_workspaces() TO onhold_app;

DROP FUNCTION admin_setup_requests(integer);
DROP FUNCTION admin_resolve_setup_request(uuid);
DROP FUNCTION admin_set_platform_setting(text, jsonb);
DROP TABLE platform_settings;
DROP TABLE setup_requests;
ALTER TABLE workspaces
    DROP COLUMN presets, DROP COLUMN extras, DROP COLUMN vocabulary,
    DROP COLUMN setup, DROP COLUMN setup_at;
UPDATE workspaces SET enabled_modules = array_remove(enabled_modules, 'finance');
