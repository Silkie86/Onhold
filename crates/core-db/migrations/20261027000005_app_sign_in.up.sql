-- Signing in the desktop and mobile apps.
--
-- The apps never see a password. An app asks for a sign-in, shows a short
-- code and opens the browser; the person signs in there as usual (password,
-- emailed link, Google, Microsoft, second factor), types the code and
-- approves. The app, polling with a secret only it holds, then receives a
-- session of its own. This is the device authorization flow of RFC 8628.
--
-- App sessions are ordinary rows in `sessions`, marked `client = 'app'` with
-- the device's name, so they show in the session list and can be signed out
-- from the browser, like a lost phone.

ALTER TABLE sessions
    ADD COLUMN client text NOT NULL DEFAULT 'browser' CHECK (client IN ('browser', 'app')),
    ADD COLUMN device_name text CHECK (length(device_name) <= 100);

CREATE TABLE app_sign_ins (
    id          uuid PRIMARY KEY,
    -- What the person types in the browser: 8 letters, shown as ABCD-EFGH.
    user_code   text NOT NULL UNIQUE CHECK (user_code ~ '^[A-Z]{8}$'),
    -- SHA-256 of the secret the app polls with.
    poll_hash   bytea NOT NULL UNIQUE,
    device_name text NOT NULL CHECK (length(btrim(device_name)) BETWEEN 1 AND 100),
    ip          inet,
    user_agent  text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    decided_by  uuid REFERENCES users (id),
    decided_at  timestamptz,
    approved    boolean,
    collected_at timestamptz,
    CHECK ((decided_at IS NULL) = (approved IS NULL)),
    CHECK ((decided_at IS NULL) = (decided_by IS NULL))
);
CREATE INDEX app_sign_ins_expires ON app_sign_ins (expires_at);

-- No grants or policies: the app role reaches this table only through the
-- functions below, each of which touches one row named by an exact key.
ALTER TABLE app_sign_ins ENABLE ROW LEVEL SECURITY;

-- Starts a sign-in, clearing out requests that expired over a day ago.
CREATE FUNCTION app_sign_in_start(p_id uuid, p_user_code text, p_poll_hash bytea,
                                  p_device_name text, p_ip inet, p_user_agent text,
                                  p_expires_at timestamptz)
    RETURNS void
    LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        DELETE FROM app_sign_ins WHERE expires_at < now() - interval '1 day';
        INSERT INTO app_sign_ins (id, user_code, poll_hash, device_name, ip, user_agent, expires_at)
        VALUES (p_id, p_user_code, p_poll_hash, p_device_name, p_ip, p_user_agent, p_expires_at);
    $$;

-- A pending, unexpired request by its code, for the approval page. Only a
-- signed-in person may look one up.
CREATE FUNCTION app_sign_in_find(p_user_code text)
    RETURNS TABLE (id uuid, device_name text, ip inet, user_agent text,
                   created_at timestamptz, expires_at timestamptz)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT a.id, a.device_name, a.ip, a.user_agent, a.created_at, a.expires_at
        FROM app_sign_ins a
        WHERE a.user_code = p_user_code
          AND a.decided_at IS NULL
          AND a.expires_at > now()
          AND app_user_id() IS NOT NULL
    $$;

-- Approves or refuses a pending request as the signed-in person. Returns
-- false if there is no such pending request.
CREATE FUNCTION app_sign_in_decide(p_user_code text, p_approve boolean)
    RETURNS boolean
    LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
BEGIN
    IF app_user_id() IS NULL THEN
        RETURN false;
    END IF;
    UPDATE app_sign_ins
    SET decided_by = app_user_id(), decided_at = now(), approved = p_approve
    WHERE user_code = p_user_code AND decided_at IS NULL AND expires_at > now();
    RETURN FOUND;
END
$$;

-- What the polling app learns: 'pending', 'denied', 'expired', or
-- 'approved' with the user, exactly once. Collected and unknown requests
-- read as 'expired'.
CREATE FUNCTION app_sign_in_collect(p_poll_hash bytea)
    RETURNS TABLE (status text, user_id uuid, device_name text)
    LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
DECLARE
    r app_sign_ins%ROWTYPE;
BEGIN
    SELECT * INTO r FROM app_sign_ins a WHERE a.poll_hash = p_poll_hash FOR UPDATE;
    IF NOT FOUND OR r.collected_at IS NOT NULL THEN
        RETURN QUERY SELECT 'expired'::text, NULL::uuid, NULL::text;
    ELSIF r.approved IS FALSE THEN
        RETURN QUERY SELECT 'denied'::text, NULL::uuid, NULL::text;
    ELSIF r.approved IS TRUE THEN
        -- Approved before it expired: still collectable for a short while after.
        IF r.decided_at < now() - interval '10 minutes' THEN
            RETURN QUERY SELECT 'expired'::text, NULL::uuid, NULL::text;
        ELSE
            UPDATE app_sign_ins SET collected_at = now() WHERE id = r.id;
            RETURN QUERY SELECT 'approved'::text, r.decided_by, r.device_name;
        END IF;
    ELSIF r.expires_at <= now() THEN
        RETURN QUERY SELECT 'expired'::text, NULL::uuid, NULL::text;
    ELSE
        RETURN QUERY SELECT 'pending'::text, NULL::uuid, NULL::text;
    END IF;
END
$$;

REVOKE ALL ON FUNCTION app_sign_in_start(uuid, text, bytea, text, inet, text, timestamptz),
    app_sign_in_find(text), app_sign_in_decide(text, boolean), app_sign_in_collect(bytea) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app_sign_in_start(uuid, text, bytea, text, inet, text, timestamptz),
    app_sign_in_find(text), app_sign_in_decide(text, boolean), app_sign_in_collect(bytea) TO onhold_app;

-- The session lookup also says which kind of client a session belongs to,
-- so a browser cookie can never carry an app's session and vice versa.
DROP FUNCTION auth_find_session(bytea);
CREATE FUNCTION auth_find_session(p_token_hash bytea)
    RETURNS TABLE (session_id uuid, user_id uuid, csrf_token text, mfa_verified boolean,
                   expires_at timestamptz, impersonated_workspace_id uuid,
                   impersonation_expires_at timestamptz, client text)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT s.id, s.user_id, s.csrf_token, s.mfa_verified, s.expires_at,
               s.impersonated_workspace_id, s.impersonation_expires_at, s.client
        FROM sessions s JOIN users u ON u.id = s.user_id
        WHERE s.token_hash = p_token_hash
          AND s.revoked_at IS NULL
          AND s.expires_at > now()
          AND u.deleted_at IS NULL
    $$;
REVOKE ALL ON FUNCTION auth_find_session(bytea) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION auth_find_session(bytea) TO onhold_app;
