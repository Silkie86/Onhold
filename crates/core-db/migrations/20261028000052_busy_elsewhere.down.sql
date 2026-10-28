DROP FUNCTION IF EXISTS my_other_talent_workspaces();
DROP FUNCTION IF EXISTS busy_elsewhere(uuid[], timestamptz, timestamptz);
REVOKE UPDATE (share_busy) ON users FROM onhold_app;
ALTER TABLE users DROP COLUMN IF EXISTS share_busy;
