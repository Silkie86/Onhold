DROP INDEX IF EXISTS payments_organisation;
DROP INDEX IF EXISTS invoices_payer_contact;
DROP INDEX IF EXISTS enquiries_venue;
DROP INDEX IF EXISTS bookings_venue;
DROP INDEX IF EXISTS bookings_contact;
DROP TABLE IF EXISTS import_records;
UPDATE imports SET status = coalesce(undo_from, 'done') WHERE status IN ('undoing', 'undone');
ALTER TABLE imports DROP CONSTRAINT imports_status_check;
ALTER TABLE imports ADD CONSTRAINT imports_status_check
    CHECK (status IN ('uploaded', 'queued', 'running', 'done', 'failed'));
ALTER TABLE imports
    DROP COLUMN IF EXISTS undo_error,
    DROP COLUMN IF EXISTS undo_from,
    DROP COLUMN IF EXISTS undone_at,
    DROP COLUMN IF EXISTS kept_count,
    DROP COLUMN IF EXISTS undone_count;
