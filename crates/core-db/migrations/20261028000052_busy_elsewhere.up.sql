-- Busy elsewhere (M7): someone who is talent with more than one workspace
-- (two agencies, or an agency and their own) may let each see when they are
-- booked with the others: the times only, never the title, client, fee or
-- which workspace. Off until they switch it on in their portal.

ALTER TABLE users ADD COLUMN share_busy boolean NOT NULL DEFAULT false;
GRANT UPDATE (share_busy) ON users TO onhold_app;

-- Bookings (held, pending or confirmed, so pencils too) overlapping [p_from, p_to) that the
-- people behind these talent records have in other workspaces, for those
-- who share. Only talent records of the workspace in context are looked at,
-- and only the times come back.
CREATE FUNCTION busy_elsewhere(p_talent uuid[], p_from timestamptz, p_to timestamptz)
    RETURNS TABLE (talent_id uuid, talent_name text, starts_at timestamptz, ends_at timestamptz)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT DISTINCT t.id, t.name, b.starts_at, b.ends_at
        FROM talent t
        JOIN users u ON u.id = t.portal_user_id AND u.share_busy AND u.deleted_at IS NULL
        JOIN talent other ON other.portal_user_id = u.id AND other.workspace_id <> t.workspace_id
             AND other.deleted_at IS NULL
        -- A closed workspace's old bookings are no one's busy time.
        JOIN workspaces w ON w.id = other.workspace_id AND w.status <> 'closed'
        JOIN memberships m ON m.workspace_id = other.workspace_id AND m.user_id = u.id
             AND m.deleted_at IS NULL
        JOIN booking_talent bt ON bt.workspace_id = other.workspace_id AND bt.talent_id = other.id
        JOIN bookings b ON b.workspace_id = bt.workspace_id AND b.id = bt.booking_id
             AND b.deleted_at IS NULL AND b.status IN ('hold', 'pending', 'confirmed')
        WHERE t.workspace_id = app_workspace_id() AND t.id = ANY(p_talent) AND t.deleted_at IS NULL
          AND app_workspace_id() IS NOT NULL
          AND bt.during && tstzrange(p_from, p_to, '[)')
        ORDER BY 3, 2
        LIMIT 500
    $$;
REVOKE ALL ON FUNCTION busy_elsewhere(uuid[], timestamptz, timestamptz) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION busy_elsewhere(uuid[], timestamptz, timestamptz) TO onhold_app;

-- How many other workspaces the signed-in user is talent with, for the
-- switch's wording.
CREATE FUNCTION my_other_talent_workspaces()
    RETURNS bigint
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT count(DISTINCT t.workspace_id) FROM talent t
        JOIN workspaces w ON w.id = t.workspace_id AND w.status <> 'closed'
        JOIN memberships m ON m.workspace_id = t.workspace_id AND m.user_id = t.portal_user_id
             AND m.deleted_at IS NULL
        WHERE t.portal_user_id = app_user_id() AND t.deleted_at IS NULL
          AND t.workspace_id IS DISTINCT FROM app_workspace_id()
    $$;
REVOKE ALL ON FUNCTION my_other_talent_workspaces() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION my_other_talent_workspaces() TO onhold_app;
