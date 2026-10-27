-- Reverts M4: bookings and calendar.

DROP FUNCTION quote_public_find(bytea);
DROP FUNCTION calendar_feed_find(bytea);
DROP TABLE calendar_feeds;
DROP TABLE tasks;
DROP TRIGGER quotes_freeze_accepted ON quotes;
DROP FUNCTION quotes_freeze_accepted();
DROP TABLE quotes;
DROP TYPE quote_status;
DROP TABLE availability;
DROP TYPE availability_kind;
DROP TRIGGER bookings_sync_talent ON bookings;
DROP FUNCTION bookings_sync_talent();
DROP TABLE booking_talent;
DROP FUNCTION booking_talent_fill();
DROP FUNCTION booking_is_firm(bookings);
DROP TABLE bookings;
DROP TYPE booking_status;
DROP TABLE enquiry_talent;
DROP TABLE enquiries;
DROP TABLE workspace_counters;
ALTER TABLE workspaces DROP COLUMN hold_days;

DELETE FROM role_permissions WHERE permission IN (
    'enquiry:read', 'enquiry:write', 'booking:read', 'booking:write', 'booking:override',
    'quote:read', 'quote:write', 'task:read', 'task:write');

-- btree_gist stays: other databases on the server may use it, and keeping
-- an unused extension changes nothing.
