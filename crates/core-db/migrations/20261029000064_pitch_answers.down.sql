DROP TABLE pitch_not_pitches;
DROP INDEX pitch_log_unanswered;
DROP TRIGGER pitch_log_updated_at ON pitch_log;
DROP TRIGGER pitch_log_version ON pitch_log;
CREATE TRIGGER pitch_log_updated_at BEFORE UPDATE ON pitch_log FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER pitch_log_version BEFORE UPDATE ON pitch_log FOR EACH ROW EXECUTE FUNCTION bump_version();
ALTER TABLE pitch_log DROP CONSTRAINT pitch_log_answer_kind;
ALTER TABLE pitch_log DROP CONSTRAINT pitch_log_answer;
ALTER TABLE pitch_log DROP COLUMN answer_message_id, DROP COLUMN answered_at;
CREATE INDEX pitch_log_message ON pitch_log (workspace_id, message_id) WHERE message_id IS NOT NULL;
DROP INDEX pitch_log_one_per_message;
ALTER TABLE pitch_log DROP CONSTRAINT pitch_log_sent_message;
ALTER TABLE pitch_log DROP CONSTRAINT pitch_log_source_check;
-- Mail logged from Sent did not go through OnHold: it stays as logged by hand.
UPDATE pitch_log SET source = 'manual', message_id = NULL WHERE source = 'spotted';
ALTER TABLE pitch_log ADD CONSTRAINT pitch_log_source_check CHECK (source IN ('manual', 'sent'));
ALTER TABLE pitch_log ADD CONSTRAINT pitch_log_sent_message CHECK ((source = 'sent') = (message_id IS NOT NULL));
