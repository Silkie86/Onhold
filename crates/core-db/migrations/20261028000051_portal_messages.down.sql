DELETE FROM thread_message_files WHERE message_id IN (
    SELECT m.id FROM thread_messages m JOIN threads t ON t.id = m.thread_id WHERE t.channel = 'portal');
DELETE FROM thread_messages WHERE thread_id IN (SELECT id FROM threads WHERE channel = 'portal');
DELETE FROM threads WHERE channel = 'portal';
DROP INDEX IF EXISTS threads_portal;
ALTER TABLE threads DROP CONSTRAINT IF EXISTS threads_portal_check;
ALTER TABLE threads DROP CONSTRAINT IF EXISTS threads_talent_fk;
ALTER TABLE threads DROP COLUMN IF EXISTS portal_read_at;
ALTER TABLE threads DROP COLUMN IF EXISTS portal_user_id;
ALTER TABLE threads DROP COLUMN IF EXISTS talent_id;
ALTER TABLE threads DROP CONSTRAINT threads_messaging_check;
ALTER TABLE threads ADD CONSTRAINT threads_messaging_check
    CHECK ((channel = 'email') = (messaging_channel_id IS NULL));
ALTER TABLE threads DROP CONSTRAINT threads_channel_check;
ALTER TABLE threads ADD CONSTRAINT threads_channel_check CHECK (channel IN ('email', 'sms', 'whatsapp'));
