-- Restores idem_claim from 20261027000002, which keeps abandoned claims for 24 hours.

CREATE OR REPLACE FUNCTION idem_claim(p_scope bytea, p_key text, p_request_hash bytea)
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
