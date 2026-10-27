DROP FUNCTION IF EXISTS idem_release(bytea, text);
DROP FUNCTION IF EXISTS idem_complete(bytea, text, smallint, text, bytea);
DROP FUNCTION IF EXISTS idem_claim(bytea, text, bytea);
DROP FUNCTION IF EXISTS admin_list_workspaces();
DROP FUNCTION IF EXISTS auth_find_oidc_identity(text, text);
DROP FUNCTION IF EXISTS auth_find_magic_link(bytea);
DROP FUNCTION IF EXISTS auth_find_invitation(bytea);
DROP FUNCTION IF EXISTS auth_find_api_key(uuid);
DROP FUNCTION IF EXISTS auth_find_user_by_email(citext);
DROP FUNCTION IF EXISTS auth_find_session(bytea);

-- These policies reference memberships, so they go before it.
DROP POLICY IF EXISTS users_select ON users;
DROP POLICY IF EXISTS workspaces_select ON workspaces;

DROP TABLE IF EXISTS idempotency_keys;
DROP TABLE IF EXISTS outbox;
DROP TABLE IF EXISTS audit_log;
DROP TABLE IF EXISTS oidc_identities;
DROP TABLE IF EXISTS magic_links;
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS api_keys;
DROP TABLE IF EXISTS invitations;
DROP TABLE IF EXISTS role_permissions;
DROP TABLE IF EXISTS memberships;
DROP TABLE IF EXISTS workspaces;
DROP TABLE IF EXISTS user_recovery_codes;
DROP TABLE IF EXISTS users;

DROP TYPE IF EXISTS member_role;
DROP TYPE IF EXISTS workspace_perspective;
DROP TYPE IF EXISTS workspace_status;

DROP FUNCTION IF EXISTS bump_version();
DROP FUNCTION IF EXISTS set_updated_at();
DROP FUNCTION IF EXISTS app_user_id();
DROP FUNCTION IF EXISTS app_workspace_id();

-- The onhold_app role is cluster-wide and may be used by other databases on
-- the same cluster, so it is left in place; only this database's grant goes.
REVOKE USAGE ON SCHEMA public FROM onhold_app;
