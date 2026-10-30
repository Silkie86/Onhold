UPDATE tasks SET related_entity = NULL, related_id = NULL WHERE related_entity = 'project';
ALTER TABLE tasks DROP CONSTRAINT tasks_related_entity_check;
ALTER TABLE tasks ADD CONSTRAINT tasks_related_entity_check
    CHECK (related_entity IN ('talent', 'organisation', 'contact', 'enquiry', 'booking', 'quote',
                              'invoice', 'document'));
DELETE FROM travel_items WHERE project_id IS NOT NULL;
DELETE FROM money_lines WHERE project_id IS NOT NULL;
ALTER TABLE travel_items DROP COLUMN project_id;
ALTER TABLE travel_items ALTER COLUMN booking_id SET NOT NULL;
ALTER TABLE money_lines DROP COLUMN project_id;
ALTER TABLE money_lines ALTER COLUMN booking_id SET NOT NULL;
DROP TABLE IF EXISTS project_crew;
DROP TABLE IF EXISTS project_bookings;
DROP TABLE IF EXISTS projects;
DELETE FROM role_permissions WHERE permission IN ('project:read', 'project:write');
