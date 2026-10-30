DROP TRIGGER IF EXISTS booking_talent_answer_reset ON booking_talent;
DROP FUNCTION IF EXISTS booking_talent_answer_reset();
ALTER TABLE booking_talent
    DROP CONSTRAINT IF EXISTS booking_talent_answer,
    DROP COLUMN IF EXISTS answered_by,
    DROP COLUMN IF EXISTS answered_at,
    DROP COLUMN IF EXISTS answer_note,
    DROP COLUMN IF EXISTS answer;
ALTER TABLE workspaces DROP COLUMN IF EXISTS portal;
DROP INDEX IF EXISTS talent_portal_user;
ALTER TABLE invitations DROP CONSTRAINT IF EXISTS invitations_talent_role;
ALTER TABLE invitations DROP CONSTRAINT IF EXISTS invitations_talent_fk;
ALTER TABLE invitations DROP COLUMN IF EXISTS talent_id;
DELETE FROM role_permissions WHERE permission = 'portal:use';
INSERT INTO role_permissions (workspace_id, role, permission)
SELECT id, 'talent', 'workspace:read' FROM workspaces
ON CONFLICT DO NOTHING;
