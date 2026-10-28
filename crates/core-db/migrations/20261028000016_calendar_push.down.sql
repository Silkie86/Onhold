CREATE OR REPLACE FUNCTION integration_account_in_use(p_provider text, p_account text, p_except uuid)
    RETURNS boolean
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT EXISTS (
            SELECT 1 FROM integrations
            WHERE provider = p_provider AND account = p_account AND id <> p_except
              AND disconnected_at IS NULL
        ) OR EXISTS (
            SELECT 1 FROM talent_calendars
            WHERE provider = p_provider AND account = p_account AND id <> p_except
              AND disconnected_at IS NULL
        )
    $$;
DROP TABLE IF EXISTS calendar_push_events;
DROP TABLE IF EXISTS calendar_pushes;
