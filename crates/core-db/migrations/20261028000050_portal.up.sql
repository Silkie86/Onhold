-- The talent portal (M7): a free viewer for the artists, performers and
-- other creatives a workspace represents, invited from their talent record.
--
-- A talent member is an ordinary membership with the `talent` role. What they
-- may see is narrowed twice: their role grants only `portal:use` (no staff
-- permission reads anything else), and every portal query names only the
-- talent records whose `portal_user_id` is the signed-in user. Owners and
-- admins hold every permission, so one who is also the artist sees their own.

INSERT INTO role_permissions (workspace_id, role, permission)
SELECT w.id, r.role::member_role, 'portal:use'
FROM workspaces w CROSS JOIN (VALUES ('owner'), ('admin'), ('talent')) AS r (role)
ON CONFLICT DO NOTHING;

-- An invitation for the portal names the talent record it opens.
ALTER TABLE invitations ADD COLUMN talent_id uuid;
ALTER TABLE invitations ADD CONSTRAINT invitations_talent_fk
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id);
ALTER TABLE invitations ADD CONSTRAINT invitations_talent_role
    CHECK ((role = 'talent') = (talent_id IS NOT NULL));
CREATE INDEX talent_portal_user ON talent (workspace_id, portal_user_id)
    WHERE portal_user_id IS NOT NULL AND deleted_at IS NULL;

-- What the workspace lets its talent see and do: {"show_fees": false, ...}.
-- Unknown keys are dropped by the API; missing keys take their defaults.
ALTER TABLE workspaces ADD COLUMN portal jsonb NOT NULL DEFAULT '{}'
    CHECK (jsonb_typeof(portal) = 'object');
GRANT UPDATE (portal) ON workspaces TO onhold_app;

-- Each talent's own answer to a date: confirmed or declined, from the portal.
-- It does not move the booking (deposits, contracts and clash checks stay the
-- agency's); it tells the agency, and is cleared when the date's time moves.
ALTER TABLE booking_talent
    ADD COLUMN answer      text CHECK (answer IN ('confirmed', 'declined')),
    ADD COLUMN answer_note text CHECK (length(answer_note) <= 500),
    ADD COLUMN answered_at timestamptz,
    ADD COLUMN answered_by uuid REFERENCES users (id),
    ADD CONSTRAINT booking_talent_answer CHECK ((answer IS NULL) = (answered_at IS NULL));

CREATE FUNCTION booking_talent_answer_reset() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.during IS DISTINCT FROM OLD.during AND NOT isempty(OLD.during) THEN
        NEW.answer := NULL;
        NEW.answer_note := NULL;
        NEW.answered_at := NULL;
        NEW.answered_by := NULL;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER booking_talent_answer_reset BEFORE UPDATE OF during ON booking_talent
    FOR EACH ROW EXECUTE FUNCTION booking_talent_answer_reset();
