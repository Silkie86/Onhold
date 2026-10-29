-- Texts go with the channels they came through: email threads only remain.
DELETE FROM thread_message_files WHERE message_id IN (
    SELECT m.id FROM thread_messages m JOIN threads t ON t.id = m.thread_id
    WHERE t.channel IN ('sms', 'whatsapp'));
DELETE FROM thread_messages WHERE thread_id IN (SELECT id FROM threads WHERE channel IN ('sms', 'whatsapp'));
DELETE FROM threads WHERE channel IN ('sms', 'whatsapp');
DROP INDEX rules_preset;
ALTER TABLE rules DROP COLUMN preset;
DROP FUNCTION messaging_channel_for_hook(text);
DROP INDEX thread_messages_messaging;
ALTER TABLE thread_messages DROP CONSTRAINT thread_messages_messaging_channel_fk;
ALTER TABLE thread_messages DROP COLUMN messaging_channel_id;
DROP INDEX threads_messaging;
ALTER TABLE threads DROP CONSTRAINT threads_messaging_check;
ALTER TABLE threads DROP CONSTRAINT threads_messaging_channel_fk;
ALTER TABLE threads DROP COLUMN messaging_channel_id;
ALTER TABLE threads DROP CONSTRAINT threads_channel_check;
ALTER TABLE threads ADD CONSTRAINT threads_channel_check CHECK (channel IN ('email'));
DROP TABLE messaging_opt_outs;
DROP TABLE messaging_channels;
