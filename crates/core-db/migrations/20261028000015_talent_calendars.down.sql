CREATE OR REPLACE FUNCTION integration_account_in_use(p_provider text, p_account text, p_except uuid)
    RETURNS boolean
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT EXISTS (
            SELECT 1 FROM integrations
            WHERE provider = p_provider AND account = p_account AND id <> p_except
              AND disconnected_at IS NULL
        )
    $$;
DROP FUNCTION IF EXISTS calendar_connect_link_find(bytea);
DROP TABLE IF EXISTS calendar_busy;
DROP TABLE IF EXISTS talent_calendars;
DROP TABLE IF EXISTS calendar_connect_links;
