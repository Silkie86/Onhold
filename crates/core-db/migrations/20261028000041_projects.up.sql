-- Projects: a run of dates with its own budget, crew, costs and milestone
-- tasks, looked at as one thing. A tour is the obvious one, but the core
-- never says so: a theatre season, a speaking circuit, a travelling
-- exhibition, a shoot schedule or a release campaign are projects too, each
-- field using its own words for it.
--
-- Its dates are ordinary bookings (holds, contracts, invoices and clash
-- checks unchanged), linked here. Travel and money lines can sit on the
-- project itself for what belongs to no one date: the drive between two
-- cities on a day off, rehearsals, insurance, a tour manager's wage.

INSERT INTO role_permissions (workspace_id, role, permission)
SELECT w.id, r.role::member_role, r.permission
FROM workspaces w
CROSS JOIN (VALUES
    ('owner', 'project:read'), ('admin', 'project:read'), ('agent', 'project:read'),
    ('finance', 'project:read'), ('viewer', 'project:read'),
    ('owner', 'project:write'), ('admin', 'project:write'), ('agent', 'project:write'),
    ('finance', 'project:write')
) AS r (role, permission)
ON CONFLICT DO NOTHING;

CREATE TABLE projects (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    kind         text NOT NULL DEFAULT 'tour'
                     CHECK (kind IN ('tour', 'season', 'circuit', 'exhibition', 'production', 'campaign', 'other')),
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    status       text NOT NULL DEFAULT 'planning'
                     CHECK (status IN ('planning', 'confirmed', 'under_way', 'done', 'cancelled')),
    -- What the project's sums are shown in.
    currency     char(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    owner_id     uuid REFERENCES users (id),
    -- The expected costs, per cost category: {"travel": 250000, "crew": 120000}.
    budget       jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(budget) = 'object'),
    notes        text CHECK (length(notes) <= 20000),
    tags         text[] NOT NULL DEFAULT '{}',
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    deleted_at   timestamptz,
    UNIQUE (workspace_id, id)
);
CREATE INDEX projects_workspace ON projects (workspace_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE TRIGGER projects_updated_at BEFORE UPDATE ON projects FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER projects_version BEFORE UPDATE ON projects FOR EACH ROW EXECUTE FUNCTION bump_version();

-- A project's dates. A booking belongs to one project at most.
CREATE TABLE project_bookings (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    project_id   uuid NOT NULL,
    booking_id   uuid NOT NULL PRIMARY KEY,
    FOREIGN KEY (workspace_id, project_id) REFERENCES projects (workspace_id, id),
    FOREIGN KEY (workspace_id, booking_id) REFERENCES bookings (workspace_id, id)
);
CREATE INDEX project_bookings_project ON project_bookings (workspace_id, project_id);

-- Who is on the road or on the job: a contact sheet, not accounts.
CREATE TABLE project_crew (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    project_id   uuid NOT NULL,
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    role         text CHECK (length(role) <= 100),
    phone        text CHECK (length(phone) <= 50),
    email        text CHECK (length(email) <= 320),
    -- Dietary needs, access, anything people should know.
    notes        text CHECK (length(notes) <= 2000),
    position     integer NOT NULL DEFAULT 0,
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    deleted_at   timestamptz,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, project_id) REFERENCES projects (workspace_id, id)
);
CREATE INDEX project_crew_project ON project_crew (workspace_id, project_id) WHERE deleted_at IS NULL;
CREATE TRIGGER project_crew_updated_at BEFORE UPDATE ON project_crew FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER project_crew_version BEFORE UPDATE ON project_crew FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Travel and money that belong to the project rather than one date.
ALTER TABLE travel_items ALTER COLUMN booking_id DROP NOT NULL;
ALTER TABLE travel_items ADD COLUMN project_id uuid;
ALTER TABLE travel_items ADD FOREIGN KEY (workspace_id, project_id) REFERENCES projects (workspace_id, id);
ALTER TABLE travel_items ADD CHECK ((booking_id IS NULL) <> (project_id IS NULL));
CREATE INDEX travel_items_project ON travel_items (workspace_id, project_id, starts_at) WHERE deleted_at IS NULL;

ALTER TABLE money_lines ALTER COLUMN booking_id DROP NOT NULL;
ALTER TABLE money_lines ADD COLUMN project_id uuid;
ALTER TABLE money_lines ADD FOREIGN KEY (workspace_id, project_id) REFERENCES projects (workspace_id, id);
ALTER TABLE money_lines ADD CHECK ((booking_id IS NULL) <> (project_id IS NULL));
CREATE INDEX money_lines_project ON money_lines (workspace_id, project_id) WHERE deleted_at IS NULL;

-- Milestone tasks hang off a project like any other record.
ALTER TABLE tasks DROP CONSTRAINT tasks_related_entity_check;
ALTER TABLE tasks ADD CONSTRAINT tasks_related_entity_check
    CHECK (related_entity IN ('talent', 'organisation', 'contact', 'enquiry', 'booking', 'quote',
                              'invoice', 'document', 'project'));

ALTER TABLE projects ENABLE ROW LEVEL SECURITY;
ALTER TABLE project_bookings ENABLE ROW LEVEL SECURITY;
ALTER TABLE project_crew ENABLE ROW LEVEL SECURITY;
CREATE POLICY projects_tenant ON projects FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY project_bookings_tenant ON project_bookings FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY project_crew_tenant ON project_crew FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
GRANT SELECT, INSERT, UPDATE ON projects, project_crew TO onhold_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON project_bookings TO onhold_app;
