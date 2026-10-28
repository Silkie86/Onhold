-- M6, second part: sending from a workspace's own domain. The domain is
-- added at the mail provider, which gives the DNS records (DKIM, and SPF
-- with a bounce MX) the owner adds by hand or through Domain Connect;
-- once the provider finds them, the workspace's email goes out from an
-- address at that domain instead of the platform's.

CREATE TABLE sender_domains (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces (id),
    domain        text NOT NULL CHECK (length(domain) BETWEEN 3 AND 253 AND domain = lower(domain)),
    -- The provider's id for it.
    provider_id   text NOT NULL CHECK (length(provider_id) BETWEEN 1 AND 200),
    -- pending: its records are not all found yet. verified: mail can be
    -- sent from it. failed: the provider stopped looking, or its records
    -- went missing after it was verified.
    status        text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'verified', 'failed')),
    -- The records to add, with what the provider last found for each.
    records       jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(records) = 'array'),
    -- The part before the @ of the address mail is sent from.
    local_part    text NOT NULL DEFAULT 'hello'
                      CHECK (local_part ~ '^[a-z0-9._+-]{1,64}$'),
    -- The workspace sends from it (while it is verified).
    in_use        boolean NOT NULL DEFAULT false,
    -- A DMARC policy covers the domain.
    dmarc         boolean NOT NULL DEFAULT false,
    -- The domain's DNS provider, when it can add the records itself
    -- (Domain Connect).
    dns_provider  jsonb CHECK (dns_provider IS NULL OR jsonb_typeof(dns_provider) = 'object'),
    checked_at    timestamptz,
    -- When the worker next asks the provider about it.
    next_check_at timestamptz,
    verified_at   timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    version       bigint NOT NULL DEFAULT 1,
    UNIQUE (workspace_id, id)
);
-- A domain belongs to one workspace on the whole platform, as it does at
-- the provider. One never verified is removed after a while, so a domain
-- cannot be held by a workspace that does not own it.
CREATE UNIQUE INDEX sender_domains_domain ON sender_domains (domain);
CREATE UNIQUE INDEX sender_domains_in_use ON sender_domains (workspace_id) WHERE in_use;
CREATE INDEX sender_domains_workspace ON sender_domains (workspace_id, created_at);
CREATE INDEX sender_domains_due ON sender_domains (next_check_at) WHERE next_check_at IS NOT NULL;
CREATE TRIGGER sender_domains_updated_at BEFORE UPDATE ON sender_domains
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER sender_domains_version BEFORE UPDATE ON sender_domains
    FOR EACH ROW EXECUTE FUNCTION bump_version();

ALTER TABLE sender_domains ENABLE ROW LEVEL SECURITY;
CREATE POLICY sender_domains_tenant ON sender_domains FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
-- The worker's sweep finds domains due a check (ids and times only); the
-- check runs inside the workspace as the app role.
CREATE POLICY sender_domains_worker ON sender_domains FOR SELECT TO onhold_worker
    USING (next_check_at IS NOT NULL);

-- Whether a domain is taken, in any workspace, for a clear refusal.
CREATE FUNCTION sender_domain_taken(p_domain text)
    RETURNS boolean
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$ SELECT EXISTS (SELECT 1 FROM sender_domains WHERE domain = p_domain) $$;
REVOKE ALL ON FUNCTION sender_domain_taken(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION sender_domain_taken(text) TO onhold_app;

GRANT SELECT, INSERT, UPDATE, DELETE ON sender_domains TO onhold_app;
GRANT SELECT (id, workspace_id, next_check_at) ON sender_domains TO onhold_worker;
