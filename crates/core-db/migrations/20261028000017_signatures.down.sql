REVOKE UPDATE (email_signature) ON workspaces FROM onhold_app;
ALTER TABLE workspaces DROP COLUMN IF EXISTS email_signature;
ALTER TABLE memberships DROP COLUMN IF EXISTS email_signature;
