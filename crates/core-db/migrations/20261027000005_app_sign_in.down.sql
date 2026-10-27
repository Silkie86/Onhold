-- Reverts 20261027000005_app_sign_in. App sessions are signed out: without
-- the `client` column they would be indistinguishable from browser ones.

UPDATE sessions SET revoked_at = now() WHERE client = 'app' AND revoked_at IS NULL;

DROP FUNCTION IF EXISTS auth_find_session(bytea);
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
REVOKE ALL ON FUNCTION auth_find_session(bytea) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth_find_session(bytea) TO onhold_app;

DROP FUNCTION IF EXISTS app_sign_in_collect(bytea);
DROP FUNCTION IF EXISTS app_sign_in_decide(text, boolean);
DROP FUNCTION IF EXISTS app_sign_in_find(text);
DROP FUNCTION IF EXISTS app_sign_in_start(uuid, text, bytea, text, inet, text, timestamptz);
DROP TABLE IF EXISTS app_sign_ins;

ALTER TABLE sessions DROP COLUMN IF EXISTS device_name, DROP COLUMN IF EXISTS client;
