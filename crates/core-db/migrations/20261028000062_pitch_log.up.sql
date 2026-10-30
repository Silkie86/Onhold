-- Pitching, part 3: the pitch log. Each row is something done to win an
-- organisation over (pitched, followed up, called… from the workspace's own
-- list of actions) or an answer they gave (from its own list of outcomes),
-- with who at the organisation, by which channel, what was pitched, when,
-- and a short note. Logged by hand for now; sending from OnHold logs its
-- own later.

CREATE TABLE pitch_log (
    id              uuid PRIMARY KEY,
    workspace_id    uuid NOT NULL REFERENCES workspaces (id),
    organisation_id uuid NOT NULL,
    contact_id      uuid,
    kind            text NOT NULL CHECK (kind IN ('action', 'outcome')),
    -- A key from the workspace's actions or outcomes.
    type_key        text NOT NULL CHECK (type_key ~ '^[a-z0-9_]{1,40}$'),
    channel         text CHECK (channel IN ('email', 'phone', 'text', 'whatsapp', 'social', 'in_person', 'other')),
    talent_id       uuid,
    project_id      uuid,
    note            text CHECK (length(note) <= 2000),
    happened_at     timestamptz NOT NULL DEFAULT now(),
    -- `manual`: logged by hand.
    source          text NOT NULL DEFAULT 'manual' CHECK (source IN ('manual')),
    version         bigint NOT NULL DEFAULT 1,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    created_by      uuid REFERENCES users (id),
    updated_by      uuid REFERENCES users (id),
    deleted_at      timestamptz,
    CHECK (talent_id IS NULL OR project_id IS NULL),
    FOREIGN KEY (workspace_id, organisation_id) REFERENCES organisations (workspace_id, id),
    FOREIGN KEY (workspace_id, contact_id) REFERENCES contacts (workspace_id, id),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id),
    FOREIGN KEY (workspace_id, project_id) REFERENCES projects (workspace_id, id)
);
CREATE INDEX pitch_log_organisation ON pitch_log (workspace_id, organisation_id, happened_at DESC)
    WHERE deleted_at IS NULL;
CREATE INDEX pitch_log_contact ON pitch_log (workspace_id, contact_id)
    WHERE deleted_at IS NULL AND contact_id IS NOT NULL;
CREATE INDEX pitch_log_talent ON pitch_log (workspace_id, talent_id)
    WHERE deleted_at IS NULL AND talent_id IS NOT NULL;
CREATE INDEX pitch_log_project ON pitch_log (workspace_id, project_id)
    WHERE deleted_at IS NULL AND project_id IS NOT NULL;
CREATE TRIGGER pitch_log_updated_at BEFORE UPDATE ON pitch_log FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER pitch_log_version BEFORE UPDATE ON pitch_log FOR EACH ROW EXECUTE FUNCTION bump_version();

ALTER TABLE pitch_log ENABLE ROW LEVEL SECURITY;
CREATE POLICY pitch_log_tenant ON pitch_log FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
GRANT SELECT, INSERT, UPDATE ON pitch_log TO onhold_app;
