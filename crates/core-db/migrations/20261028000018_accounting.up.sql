-- M6, fourth part: a workspace's books in Xero or QuickBooks Online. What
-- the workspace issues from a chosen date on (invoices, credit notes, and
-- the payments recorded against them) is sent there; what is paid there is
-- read back; invoices from before that date can be imported as past ones.

-- The workspace's connection to its books. One live at a time. The grant
-- is encrypted with the server's keyring and bound to the row's id.
CREATE TABLE accounting_connections (
    id               uuid PRIMARY KEY,
    workspace_id     uuid NOT NULL REFERENCES workspaces (id),
    provider         text NOT NULL CHECK (provider IN ('xero', 'quickbooks')),
    -- Xero's organisation (tenant) id; QuickBooks' company (realm) id.
    tenant_id        text NOT NULL CHECK (length(tenant_id) BETWEEN 1 AND 100),
    tenant_name      text NOT NULL CHECK (length(tenant_name) BETWEEN 1 AND 300),
    -- QuickBooks works out US sales tax itself, taking only taxable or not.
    automatic_tax    boolean NOT NULL DEFAULT false,
    -- Emptied once the grant is revoked.
    secret           bytea NOT NULL,
    -- Chosen in settings: where sales go (Xero: a revenue account's code;
    -- QuickBooks: a service item), and the account payments are paid into.
    sales_account    text CHECK (length(sales_account) BETWEEN 1 AND 100),
    bank_account     text CHECK (length(bank_account) BETWEEN 1 AND 100),
    -- What is issued on or after it is sent; earlier invoices can be imported.
    start_date       date NOT NULL,
    -- active: working. failing: being retried. reconnect: only connecting
    -- again helps.
    status           text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'failing', 'reconnect')),
    failures         integer NOT NULL DEFAULT 0 CHECK (failures >= 0),
    last_error       text CHECK (length(last_error) <= 1000),
    last_error_at    timestamptz,
    last_synced_at   timestamptz,
    -- When what was paid in the books was last read back in full, and
    -- where a read-back cut short by the call budget goes on from.
    paid_checked_at  timestamptz,
    paid_cursor      uuid,
    -- Items the books refused before this are tried again (once each) by
    -- the next sync; it moves on after each full sync and on "send now".
    retry_before     timestamptz NOT NULL DEFAULT now(),
    next_sync_at     timestamptz,
    sync_lease_until timestamptz,
    -- Names the sync holding the lease; each of its steps checks it.
    sync_token       uuid,
    -- Something changed while a sync ran: another follows it at once.
    sync_again       boolean NOT NULL DEFAULT false,
    -- Importing past invoices: none asked, running, done or failed; the
    -- next page to read; how many came in.
    import_status    text NOT NULL DEFAULT 'none' CHECK (import_status IN ('none', 'running', 'done', 'failed')),
    import_page      integer NOT NULL DEFAULT 1 CHECK (import_page >= 1),
    imported         integer NOT NULL DEFAULT 0 CHECK (imported >= 0),
    import_error     text CHECK (length(import_error) <= 1000),
    -- Disconnected: the worker revokes the grant, then empties it.
    revoke_at        timestamptz,
    connected_by     uuid REFERENCES users (id),
    connected_at     timestamptz NOT NULL DEFAULT now(),
    disconnected_at  timestamptz,
    updated_at       timestamptz NOT NULL DEFAULT now(),
    version          bigint NOT NULL DEFAULT 1,
    UNIQUE (workspace_id, id),
    CHECK (disconnected_at IS NULL OR next_sync_at IS NULL)
);
CREATE UNIQUE INDEX accounting_connections_live ON accounting_connections (workspace_id)
    WHERE disconnected_at IS NULL;
CREATE INDEX accounting_connections_books ON accounting_connections (workspace_id, provider, tenant_id);
CREATE INDEX accounting_connections_due ON accounting_connections (next_sync_at) WHERE next_sync_at IS NOT NULL;
CREATE INDEX accounting_connections_revoke ON accounting_connections (revoke_at) WHERE revoke_at IS NOT NULL;
CREATE TRIGGER accounting_connections_updated_at BEFORE UPDATE ON accounting_connections
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER accounting_connections_version BEFORE UPDATE ON accounting_connections
    FOR EACH ROW EXECUTE FUNCTION bump_version();

-- What in the books stands for what in OnHold: an organisation or talent
-- (`contact`), an invoice or credit note (`invoice`), a payment's
-- allocation to an invoice (`allocation`), a credit note set against the
-- invoice it credits (`credit`), and which tax rate each tax code is
-- (`tax_code`, chosen in settings). Kept when a connection ends, so
-- connecting the same books again sends nothing twice.
CREATE TABLE accounting_links (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces (id),
    connection_id uuid NOT NULL,
    kind          text NOT NULL CHECK (kind IN ('contact', 'invoice', 'allocation', 'credit', 'tax_code')),
    local_id      uuid NOT NULL,
    -- None: nothing was sent (a voided invoice never sent, a payment the
    -- books had already).
    remote_id     text CHECK (length(remote_id) BETWEEN 1 AND 200),
    -- What was sent last (`issued`, `void`, `live`, `removed`), or
    -- `sending`: a payment or credit asked for whose answer has not come
    -- back yet, tried again with the same key and amount.
    state         text CHECK (length(state) <= 20),
    -- What a `sending` payment or credit was for.
    amount_minor  bigint CHECK (amount_minor > 0),
    -- synced: in the books as OnHold has it. failed: refused (`error` says
    -- why), tried again at the next full sync or when it changes.
    -- imported: came from the books, never sent back.
    status        text NOT NULL DEFAULT 'synced' CHECK (status IN ('synced', 'failed', 'imported')),
    error         text CHECK (length(error) <= 1000),
    -- Refusals so far: a new try after a refusal is a new request.
    attempts      integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (connection_id, kind, local_id),
    FOREIGN KEY (workspace_id, connection_id) REFERENCES accounting_connections (workspace_id, id)
);
CREATE INDEX accounting_links_remote ON accounting_links (connection_id, kind, remote_id);
CREATE INDEX accounting_links_failed ON accounting_links (connection_id) WHERE status = 'failed';
CREATE TRIGGER accounting_links_updated_at BEFORE UPDATE ON accounting_links
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

ALTER TABLE accounting_connections ENABLE ROW LEVEL SECURITY;
ALTER TABLE accounting_links ENABLE ROW LEVEL SECURITY;

CREATE POLICY accounting_connections_tenant ON accounting_connections FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY accounting_links_tenant ON accounting_links FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
-- The worker's sweep finds connections due a sync or a revoke (ids and
-- times only); the job runs inside the workspace as the app role.
CREATE POLICY accounting_connections_worker ON accounting_connections FOR SELECT TO onhold_worker
    USING (next_sync_at IS NOT NULL OR revoke_at IS NOT NULL);

-- Whether another live connection, in any workspace, reaches the same
-- books: ending OnHold's access to them (Intuit's revoke, Xero's
-- connection delete) would cut that one off too, so it is left alone.
CREATE FUNCTION accounting_books_in_use(p_provider text, p_tenant text, p_except uuid)
    RETURNS boolean
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT EXISTS (
            SELECT 1 FROM accounting_connections
            WHERE provider = p_provider AND tenant_id = p_tenant AND id <> p_except
              AND disconnected_at IS NULL
        )
    $$;
REVOKE ALL ON FUNCTION accounting_books_in_use(text, text, uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION accounting_books_in_use(text, text, uuid) TO onhold_app;

GRANT SELECT, INSERT, UPDATE ON accounting_connections, accounting_links TO onhold_app;
GRANT SELECT (id, workspace_id, next_sync_at, sync_lease_until, revoke_at) ON accounting_connections TO onhold_worker;
