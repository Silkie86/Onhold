-- Pitching, part 3b: sending pitches. A pitch sent from OnHold logs itself
-- (`sent`), pointing at the message, so its thread and whether it went are
-- a click away; any entry can say which pitch template it was written from.
-- Each member can pick how they usually send a pitch, over the workspace's
-- default.
ALTER TABLE pitch_log DROP CONSTRAINT pitch_log_source_check;
ALTER TABLE pitch_log ADD CONSTRAINT pitch_log_source_check CHECK (source IN ('manual', 'sent'));
ALTER TABLE pitch_log
    ADD COLUMN template_key text CHECK (template_key ~ '^[a-z0-9_]{1,40}$'),
    ADD COLUMN message_id uuid,
    ADD FOREIGN KEY (workspace_id, message_id) REFERENCES thread_messages (workspace_id, id),
    ADD CONSTRAINT pitch_log_sent_message CHECK ((source = 'sent') = (message_id IS NOT NULL));
CREATE INDEX pitch_log_message ON pitch_log (workspace_id, message_id) WHERE message_id IS NOT NULL;

ALTER TABLE memberships ADD COLUMN pitch_send_with text
    CHECK (pitch_send_with IN ('onhold', 'mail_app', 'copy'));
