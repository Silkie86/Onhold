-- Undoing an import: what each import created, so it can be taken away
-- again, and how far the undo got.

ALTER TABLE imports DROP CONSTRAINT imports_status_check;
ALTER TABLE imports ADD CONSTRAINT imports_status_check
    CHECK (status IN ('uploaded', 'queued', 'running', 'done', 'failed', 'undoing', 'undone'));
ALTER TABLE imports
    ADD COLUMN undone_count integer NOT NULL DEFAULT 0,
    -- Records changed since the import, or now in use, which an undo keeps.
    ADD COLUMN kept_count integer NOT NULL DEFAULT 0,
    ADD COLUMN undone_at timestamptz;

-- A record an import created. Rows go as the undo deals with them.
CREATE TABLE import_records (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    import_id    uuid NOT NULL REFERENCES imports (id) ON DELETE CASCADE,
    entity       text NOT NULL CHECK (entity IN ('talent', 'organisation', 'contact')),
    record_id    uuid NOT NULL,
    PRIMARY KEY (import_id, record_id)
);

ALTER TABLE import_records ENABLE ROW LEVEL SECURITY;
CREATE POLICY import_records_tenant ON import_records FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
GRANT SELECT, INSERT, DELETE ON import_records TO onhold_app;
