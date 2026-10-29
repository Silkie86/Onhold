-- Texts go with the channels they came through: email threads only remain.
-- Their media nothing else holds is let go, so the sweep of unattached
-- files removes it.
UPDATE files f SET attached_at = NULL
WHERE f.id IN (SELECT tmf.file_id FROM thread_message_files tmf
               JOIN thread_messages m ON m.id = tmf.message_id
               JOIN threads t ON t.id = m.thread_id
               WHERE t.channel IN ('sms', 'whatsapp') AND tmf.file_id IS NOT NULL)
  AND NOT EXISTS (SELECT 1 FROM document_versions v
                  WHERE v.workspace_id = f.workspace_id AND v.file_id = f.id)
  AND NOT EXISTS (SELECT 1 FROM pdf_imports p
                  WHERE p.workspace_id = f.workspace_id AND p.file_id = f.id)
  AND NOT EXISTS (SELECT 1 FROM thread_message_files o
                  JOIN thread_messages om ON om.id = o.message_id
                  JOIN threads ot ON ot.id = om.thread_id
                  WHERE o.workspace_id = f.workspace_id AND o.file_id = f.id
                    AND ot.channel NOT IN ('sms', 'whatsapp'));
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
