DROP INDEX IF EXISTS outbound_messages_to;
DROP INDEX IF EXISTS audit_log_object;

DROP TABLE IF EXISTS saved_views;
DROP TABLE IF EXISTS imports;
DROP TABLE IF EXISTS contacts;
DROP TABLE IF EXISTS organisations;
DROP TABLE IF EXISTS talent_members;
DROP TABLE IF EXISTS talent;
DROP TYPE IF EXISTS talent_kind;
DROP FUNCTION IF EXISTS search_join(text[]);

-- Fields, pipelines and views defined for the CRM's entities go with them.
DELETE FROM custom_field_defs WHERE entity IN ('talent', 'talent_member', 'organisation', 'contact');

DELETE FROM role_permissions WHERE permission IN (
    'talent:read', 'talent:write', 'organisation:read', 'organisation:write',
    'contact:read', 'contact:write');
