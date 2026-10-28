DROP TABLE IF EXISTS import_records;
UPDATE imports SET status = 'done' WHERE status IN ('undoing', 'undone');
ALTER TABLE imports DROP CONSTRAINT imports_status_check;
ALTER TABLE imports ADD CONSTRAINT imports_status_check
    CHECK (status IN ('uploaded', 'queued', 'running', 'done', 'failed'));
ALTER TABLE imports
    DROP COLUMN IF EXISTS undone_at,
    DROP COLUMN IF EXISTS kept_count,
    DROP COLUMN IF EXISTS undone_count;
