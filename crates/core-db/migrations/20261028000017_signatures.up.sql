-- M6: email signatures. A member's own, added to what they send from the
-- inbox and to the quotes, invoices and signing requests they send; and
-- the workspace's, used when a member has none and on emails rules send.
-- Plain text, a few lines.
ALTER TABLE memberships ADD COLUMN email_signature text
    CHECK (length(email_signature) BETWEEN 1 AND 2000);
ALTER TABLE workspaces ADD COLUMN email_signature text
    CHECK (length(email_signature) BETWEEN 1 AND 2000);
GRANT UPDATE (email_signature) ON workspaces TO onhold_app;
