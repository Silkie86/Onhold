-- Reverts the mailboxes and the unified inbox.

DROP TABLE thread_message_files;
DROP TABLE thread_messages;
DROP TABLE threads;
DROP TABLE integrations;

DELETE FROM role_permissions WHERE permission IN ('inbox:read', 'inbox:write', 'integration:manage');
