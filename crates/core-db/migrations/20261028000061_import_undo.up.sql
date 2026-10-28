-- Undoing an import: what each import created, so it can be taken away
-- again, and how far the undo got.

ALTER TABLE imports DROP CONSTRAINT imports_status_check;
ALTER TABLE imports ADD CONSTRAINT imports_status_check
    CHECK (status IN ('uploaded', 'queued', 'running', 'done', 'failed', 'undoing', 'undone'));
ALTER TABLE imports
    ADD COLUMN undone_count integer NOT NULL DEFAULT 0,
    -- Records changed since the import, or now in use, which an undo keeps.
    ADD COLUMN kept_count integer NOT NULL DEFAULT 0,
    ADD COLUMN undone_at timestamptz,
    -- What it was before an undo began, to go back to if the undo gives up,
    -- and why it gave up.
    ADD COLUMN undo_from text CHECK (undo_from IN ('done', 'failed')),
    ADD COLUMN undo_error text;

-- A record an import created, with its version once the import was done
-- with it: a later version means someone changed it since. Rows go as the
-- undo deals with them.
CREATE TABLE import_records (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    import_id    uuid NOT NULL REFERENCES imports (id) ON DELETE CASCADE,
    entity       text NOT NULL CHECK (entity IN ('talent', 'organisation', 'contact')),
    record_id    uuid NOT NULL,
    version      bigint NOT NULL,
    PRIMARY KEY (import_id, record_id)
);

ALTER TABLE import_records ENABLE ROW LEVEL SECURITY;
CREATE POLICY import_records_tenant ON import_records FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
GRANT SELECT, INSERT, UPDATE, DELETE ON import_records TO onhold_app;

-- Undo asks of each record whether anything uses it.
CREATE INDEX bookings_contact ON bookings (workspace_id, contact_id)
    WHERE deleted_at IS NULL AND contact_id IS NOT NULL;
CREATE INDEX bookings_venue ON bookings (workspace_id, venue_id)
    WHERE deleted_at IS NULL AND venue_id IS NOT NULL;
CREATE INDEX enquiries_venue ON enquiries (workspace_id, venue_id)
    WHERE deleted_at IS NULL AND venue_id IS NOT NULL;
CREATE INDEX invoices_payer_contact ON invoices (workspace_id, payer_contact_id)
    WHERE deleted_at IS NULL AND payer_contact_id IS NOT NULL;
CREATE INDEX payments_organisation ON payments (workspace_id, organisation_id)
    WHERE organisation_id IS NOT NULL;
