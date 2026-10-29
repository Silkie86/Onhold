ALTER TABLE memberships DROP COLUMN pitch_send_with;
DROP INDEX pitch_log_message;
ALTER TABLE pitch_log DROP CONSTRAINT pitch_log_sent_message;
UPDATE pitch_log SET source = 'manual' WHERE source = 'sent';
ALTER TABLE pitch_log DROP COLUMN message_id, DROP COLUMN template_key;
ALTER TABLE pitch_log DROP CONSTRAINT pitch_log_source_check;
ALTER TABLE pitch_log ADD CONSTRAINT pitch_log_source_check CHECK (source IN ('manual'));
