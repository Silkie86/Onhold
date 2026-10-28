-- Pitching, part 1: what a list of people to sell to needs on each
-- organisation, and the workspace's own settings for it.
--
-- Where an organisation is: its country (an ISO code, worked out from the
-- main address however it was typed) and city, kept as columns so long
-- lists filter quickly. Its world region comes from the country through the
-- workspace's own split of the world, unless someone picked one for it.
--
-- Where things stand: client (has worked with you) or prospect (hasn't
-- yet), which changes by itself as enquiries and confirmed bookings come
-- in; a status with a colour, from the workspace's own list; a follow-up
-- date; and "do not contact", which keeps them out of pitching altogether.
--
-- What they want: talent or projects they asked about (interested) and
-- ones the workspace thinks would suit them (potential matches), two lists
-- that are never mixed; and similar acts they book from others.

ALTER TABLE organisations
    ADD COLUMN country_code     char(2) CHECK (country_code ~ '^[A-Z]{2}$'),
    ADD COLUMN city             text CHECK (length(city) <= 200),
    -- A region picked by hand; NULL means the country's.
    ADD COLUMN region_key       text CHECK (region_key ~ '^[a-z0-9_]{1,40}$'),
    ADD COLUMN relationship     text CHECK (relationship IN ('client', 'prospect')),
    -- A key from the workspace's pitching statuses.
    ADD COLUMN status_key       text CHECK (status_key ~ '^[a-z0-9_]{1,40}$'),
    ADD COLUMN follow_up_on     date,
    ADD COLUMN do_not_contact   boolean NOT NULL DEFAULT false,
    ADD COLUMN buys_from_others text[] NOT NULL DEFAULT '{}' CHECK (cardinality(buys_from_others) <= 50);
CREATE INDEX organisations_country ON organisations (workspace_id, country_code) WHERE deleted_at IS NULL;
CREATE INDEX organisations_follow_up ON organisations (workspace_id, follow_up_on)
    WHERE follow_up_on IS NOT NULL AND deleted_at IS NULL;

-- Addresses saved before this whose country is already a two-letter code.
-- Others get their country the next time they are saved or imported.
UPDATE organisations
SET country_code = upper(btrim(addresses -> 0 ->> 'country')),
    city = nullif(btrim(addresses -> 0 ->> 'city'), '')
WHERE btrim(addresses -> 0 ->> 'country') ~* '^[a-z]{2}$';

-- Web pages and social profiles: a website, Instagram, LinkedIn…
ALTER TABLE contacts
    ADD COLUMN links text[] NOT NULL DEFAULT '{}' CHECK (cardinality(links) <= 10);

-- The talent or projects an organisation is interested in, or would suit.
CREATE TABLE organisation_items (
    workspace_id    uuid NOT NULL REFERENCES workspaces (id),
    organisation_id uuid NOT NULL,
    list            text NOT NULL CHECK (list IN ('interested', 'match')),
    talent_id       uuid,
    project_id      uuid,
    position        integer NOT NULL DEFAULT 0,
    CHECK ((talent_id IS NULL) <> (project_id IS NULL)),
    FOREIGN KEY (workspace_id, organisation_id) REFERENCES organisations (workspace_id, id),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id),
    FOREIGN KEY (workspace_id, project_id) REFERENCES projects (workspace_id, id)
);
CREATE UNIQUE INDEX organisation_items_talent ON organisation_items (organisation_id, list, talent_id)
    WHERE talent_id IS NOT NULL;
CREATE UNIQUE INDEX organisation_items_project ON organisation_items (organisation_id, list, project_id)
    WHERE project_id IS NOT NULL;
CREATE INDEX organisation_items_by_talent ON organisation_items (workspace_id, talent_id) WHERE talent_id IS NOT NULL;
CREATE INDEX organisation_items_by_project ON organisation_items (workspace_id, project_id) WHERE project_id IS NOT NULL;

-- A workspace's pitching settings: its regions, statuses and what it
-- pitches. No row means OnHold's defaults.
CREATE TABLE pitching_settings (
    workspace_id uuid PRIMARY KEY REFERENCES workspaces (id),
    settings     jsonb NOT NULL
                 CHECK (jsonb_typeof(settings) = 'object' AND octet_length(settings::text) <= 32768),
    version      bigint NOT NULL DEFAULT 1,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    updated_by   uuid REFERENCES users (id)
);
CREATE TRIGGER pitching_settings_updated_at BEFORE UPDATE ON pitching_settings FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER pitching_settings_version BEFORE UPDATE ON pitching_settings FOR EACH ROW EXECUTE FUNCTION bump_version();

ALTER TABLE organisation_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE pitching_settings ENABLE ROW LEVEL SECURITY;
CREATE POLICY organisation_items_tenant ON organisation_items FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY pitching_settings_tenant ON pitching_settings FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
-- The lists are replaced when they change; the organisation's audit entry
-- keeps the history.
GRANT SELECT, INSERT, DELETE ON organisation_items TO onhold_app;
GRANT SELECT, INSERT, UPDATE ON pitching_settings TO onhold_app;
