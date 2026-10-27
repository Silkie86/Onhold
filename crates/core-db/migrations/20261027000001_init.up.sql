-- Extensions the core relies on from M1 onwards.
-- citext: case-insensitive emails and slugs.
-- pg_trgm: fuzzy search and deduplication in the CRM.
CREATE EXTENSION IF NOT EXISTS citext;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
