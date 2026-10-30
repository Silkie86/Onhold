UPDATE workspaces SET enabled_modules = array_remove(enabled_modules, 'pitching');
DROP TABLE IF EXISTS pitching_settings;
DROP TABLE IF EXISTS organisation_items;
ALTER TABLE contacts DROP COLUMN IF EXISTS links;
DROP INDEX IF EXISTS organisations_follow_up;
DROP INDEX IF EXISTS organisations_country;
ALTER TABLE organisations
    DROP COLUMN IF EXISTS buys_from_others,
    DROP COLUMN IF EXISTS do_not_contact,
    DROP COLUMN IF EXISTS follow_up_on,
    DROP COLUMN IF EXISTS status_key,
    DROP COLUMN IF EXISTS relationship,
    DROP COLUMN IF EXISTS region_key,
    DROP COLUMN IF EXISTS city,
    DROP COLUMN IF EXISTS country_code;
