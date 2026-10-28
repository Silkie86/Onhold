-- M6, first part: people's own mailboxes and the unified inbox. A member
-- connects a Gmail, Outlook or IMAP/SMTP mailbox; the worker reads what
-- arrives into threads linked to contacts, enquiries and bookings, and
-- replies written in OnHold go out from that mailbox.

-- Permissions -------------------------------------------------------------------

INSERT INTO role_permissions (workspace_id, role, permission)
SELECT w.id, r.role::member_role, r.permission
FROM workspaces w
CROSS JOIN (VALUES
    ('owner', 'inbox:read'), ('admin', 'inbox:read'), ('agent', 'inbox:read'),
    ('finance', 'inbox:read'), ('viewer', 'inbox:read'),
    ('owner', 'inbox:write'), ('admin', 'inbox:write'), ('agent', 'inbox:write'),
    ('finance', 'inbox:write'),
    ('owner', 'integration:manage'), ('admin', 'integration:manage')
) AS r (role, permission)
ON CONFLICT DO NOTHING;

-- Connected accounts ------------------------------------------------------------

-- An account a member connected: their mailbox now, their calendar and the
-- workspace's accounting system later. The tokens (or the IMAP password)
-- are encrypted with the server's keyring and bound to the row's id.
CREATE TABLE integrations (
    id              uuid PRIMARY KEY,
    workspace_id    uuid NOT NULL REFERENCES workspaces (id),
    provider        text NOT NULL CHECK (provider IN ('google', 'microsoft', 'imap')),
    purposes        text[] NOT NULL CHECK (cardinality(purposes) BETWEEN 1 AND 2
                        AND purposes <@ ARRAY['mail', 'calendar']),
    -- Who connected it; their grant is what it acts with.
    owner_id        uuid NOT NULL REFERENCES users (id),
    -- The mailbox's address, lower-cased.
    account         text NOT NULL CHECK (length(account) BETWEEN 3 AND 320 AND account = lower(account)),
    -- A Microsoft 365 shared mailbox read and sent from through the owner's grant.
    shared_mailbox  text CHECK (length(shared_mailbox) BETWEEN 3 AND 320 AND shared_mailbox = lower(shared_mailbox)),
    -- Everyone who can read the inbox sees this mailbox's mail; otherwise
    -- only its owner does.
    shared          boolean NOT NULL DEFAULT false,
    display_name    text CHECK (length(display_name) <= 200),
    -- Provider settings that are not secret (IMAP and SMTP servers).
    settings        jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(settings) = 'object'),
    secret          bytea NOT NULL,
    scopes          text[] NOT NULL DEFAULT '{}',
    -- active: working. failing: the last tries failed and will be tried
    -- again. reconnect: the grant is gone, only connecting again helps.
    status          text NOT NULL DEFAULT 'active'
                        CHECK (status IN ('active', 'failing', 'reconnect')),
    failures        integer NOT NULL DEFAULT 0 CHECK (failures >= 0),
    last_error      text CHECK (length(last_error) <= 1000),
    last_error_at   timestamptz,
    last_synced_at  timestamptz,
    -- Where reading the mailbox has got to (the provider's own format).
    mail_cursor     text CHECK (length(mail_cursor) <= 8000),
    -- Mail older than this is not read in.
    mail_since      timestamptz NOT NULL,
    -- When the worker next reads it; NULL while it is not read.
    next_poll_at    timestamptz,
    -- A read is under way until then (a worker that stopped mid-read
    -- leaves it to run out), so two reads never overlap.
    poll_lease_until timestamptz,
    connected_at    timestamptz NOT NULL DEFAULT now(),
    disconnected_at timestamptz,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    version         bigint NOT NULL DEFAULT 1,
    UNIQUE (workspace_id, id),
    CHECK (shared_mailbox IS NULL OR provider = 'microsoft')
);
-- One live connection per mailbox in a workspace.
CREATE UNIQUE INDEX integrations_live ON integrations (workspace_id, provider, coalesce(shared_mailbox, account))
    WHERE disconnected_at IS NULL;
CREATE INDEX integrations_workspace ON integrations (workspace_id, connected_at);
CREATE INDEX integrations_due ON integrations (next_poll_at)
    WHERE disconnected_at IS NULL AND next_poll_at IS NOT NULL;
CREATE TRIGGER integrations_updated_at BEFORE UPDATE ON integrations
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER integrations_version BEFORE UPDATE ON integrations
    FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Threads and messages ------------------------------------------------------------

-- A conversation. Email only for now; SMS and WhatsApp join it later.
CREATE TABLE threads (
    id               uuid PRIMARY KEY,
    workspace_id     uuid NOT NULL REFERENCES workspaces (id),
    channel          text NOT NULL DEFAULT 'email' CHECK (channel IN ('email')),
    -- The mailbox it lives in. NULL for mail sent through OnHold's own
    -- sender because no mailbox was connected.
    integration_id   uuid,
    -- The provider's conversation id (Gmail thread, Outlook conversation).
    external_id      text CHECK (length(external_id) <= 500),
    subject          text CHECK (length(subject) <= 1000),
    contact_id       uuid,
    organisation_id  uuid,
    enquiry_id       uuid,
    booking_id       uuid,
    status           text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'snoozed', 'resolved')),
    snoozed_until    timestamptz,
    assigned_to      uuid REFERENCES users (id),
    unread           boolean NOT NULL DEFAULT true,
    message_count    integer NOT NULL DEFAULT 0 CHECK (message_count >= 0),
    -- The other people in it, lower-cased.
    participants     text[] NOT NULL DEFAULT '{}' CHECK (cardinality(participants) <= 100),
    preview          text CHECK (length(preview) <= 300),
    last_message_at  timestamptz NOT NULL DEFAULT now(),
    last_inbound_at  timestamptz,
    last_outbound_at timestamptz,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    version          bigint NOT NULL DEFAULT 1,
    search_text      text GENERATED ALWAYS AS (lower(
        coalesce(subject, '') || ' ' || search_join(participants))) STORED,
    UNIQUE (workspace_id, id),
    CHECK ((status = 'snoozed') = (snoozed_until IS NOT NULL)),
    FOREIGN KEY (workspace_id, integration_id) REFERENCES integrations (workspace_id, id),
    FOREIGN KEY (workspace_id, contact_id) REFERENCES contacts (workspace_id, id),
    FOREIGN KEY (workspace_id, organisation_id) REFERENCES organisations (workspace_id, id),
    FOREIGN KEY (workspace_id, enquiry_id) REFERENCES enquiries (workspace_id, id),
    FOREIGN KEY (workspace_id, booking_id) REFERENCES bookings (workspace_id, id)
);
CREATE INDEX threads_workspace ON threads (workspace_id, last_message_at DESC);
CREATE UNIQUE INDEX threads_external ON threads (integration_id, external_id) WHERE external_id IS NOT NULL;
CREATE INDEX threads_contact ON threads (workspace_id, contact_id) WHERE contact_id IS NOT NULL;
CREATE INDEX threads_organisation ON threads (workspace_id, organisation_id) WHERE organisation_id IS NOT NULL;
CREATE INDEX threads_enquiry ON threads (workspace_id, enquiry_id) WHERE enquiry_id IS NOT NULL;
CREATE INDEX threads_booking ON threads (workspace_id, booking_id) WHERE booking_id IS NOT NULL;
CREATE INDEX threads_snoozed ON threads (snoozed_until) WHERE status = 'snoozed';
CREATE INDEX threads_search ON threads USING gin (search_text gin_trgm_ops);
CREATE TRIGGER threads_updated_at BEFORE UPDATE ON threads
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER threads_version BEFORE UPDATE ON threads
    FOR EACH ROW EXECUTE FUNCTION bump_version();

-- One message in a thread: read from a mailbox, or written in OnHold and sent.
CREATE TABLE thread_messages (
    id             uuid PRIMARY KEY,
    workspace_id   uuid NOT NULL REFERENCES workspaces (id),
    thread_id      uuid NOT NULL,
    direction      text NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    -- Copied from the thread, for the uniqueness checks below.
    integration_id uuid,
    -- The provider's id (Gmail id, Graph id, IMAP uidvalidity:uid).
    external_id    text CHECK (length(external_id) <= 500),
    -- The RFC 5322 Message-ID, without angle brackets.
    message_id     text CHECK (length(message_id) <= 998),
    in_reply_to    text CHECK (length(in_reply_to) <= 998),
    "references"   text[] NOT NULL DEFAULT '{}' CHECK (cardinality("references") <= 100),
    from_address   text CHECK (length(from_address) <= 320),
    from_name      text CHECK (length(from_name) <= 200),
    -- Where an answer goes, when the sender asked for somewhere else (web
    -- forms and booking sites send from their own address).
    reply_to       text CHECK (length(reply_to) <= 320),
    to_addresses   text[] NOT NULL DEFAULT '{}' CHECK (cardinality(to_addresses) <= 100),
    cc_addresses   text[] NOT NULL DEFAULT '{}' CHECK (cardinality(cc_addresses) <= 100),
    subject        text CHECK (length(subject) <= 1000),
    body_text      text NOT NULL DEFAULT '' CHECK (length(body_text) <= 200000),
    -- Sanitised before it is stored: no scripts, styles, forms or remote content.
    body_html      text CHECK (length(body_html) <= 500000),
    sent_at        timestamptz NOT NULL,
    -- received: read from a mailbox. queued, sending (handed to the
    -- provider, answer not yet recorded), sent, failed: written here.
    status         text NOT NULL CHECK (status IN ('received', 'queued', 'sending', 'sent', 'failed')),
    error          text CHECK (length(error) <= 1000),
    -- An auto-reply, bounce or other machine-sent message.
    automatic      boolean NOT NULL DEFAULT false,
    -- Sent to a mailing list.
    bulk           boolean NOT NULL DEFAULT false,
    -- Who wrote it in OnHold.
    author_id      uuid REFERENCES users (id),
    template_key   text CHECK (length(template_key) <= 100),
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    CHECK ((direction = 'inbound') = (status = 'received')),
    FOREIGN KEY (workspace_id, thread_id) REFERENCES threads (workspace_id, id),
    FOREIGN KEY (workspace_id, integration_id) REFERENCES integrations (workspace_id, id)
);
CREATE INDEX thread_messages_thread ON thread_messages (thread_id, sent_at, id);
-- Each provider message is read in once, and a message sent from here is
-- recognised when the mailbox's own copy of it is read back.
CREATE UNIQUE INDEX thread_messages_external ON thread_messages (integration_id, external_id)
    WHERE external_id IS NOT NULL;
CREATE UNIQUE INDEX thread_messages_message_id ON thread_messages (integration_id, message_id)
    WHERE message_id IS NOT NULL AND integration_id IS NOT NULL;
-- Threading a reply by the message it answers.
CREATE INDEX thread_messages_lookup ON thread_messages (workspace_id, message_id) WHERE message_id IS NOT NULL;

-- Files that came with a message, or were sent with one. What was sent from
-- OnHold is a stored file; what came from a mailbox stays there (only its
-- name and size are kept) and is read again when someone opens it.
CREATE TABLE thread_message_files (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    message_id   uuid NOT NULL,
    file_id      uuid,
    -- Which attachment of the message it is, counting from 0.
    part         integer CHECK (part >= 0),
    filename     text NOT NULL CHECK (length(filename) <= 255),
    content_type text NOT NULL CHECK (length(content_type) <= 255),
    size_bytes   bigint NOT NULL CHECK (size_bytes >= 0),
    CHECK ((file_id IS NULL) <> (part IS NULL)),
    FOREIGN KEY (workspace_id, message_id) REFERENCES thread_messages (workspace_id, id),
    FOREIGN KEY (workspace_id, file_id) REFERENCES files (workspace_id, id)
);
CREATE INDEX thread_message_files_message ON thread_message_files (message_id);

-- Row-level security ------------------------------------------------------------

ALTER TABLE integrations ENABLE ROW LEVEL SECURITY;
ALTER TABLE threads ENABLE ROW LEVEL SECURITY;
ALTER TABLE thread_messages ENABLE ROW LEVEL SECURITY;
ALTER TABLE thread_message_files ENABLE ROW LEVEL SECURITY;

CREATE POLICY integrations_tenant ON integrations FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY threads_tenant ON threads FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY thread_messages_tenant ON thread_messages FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY thread_message_files_tenant ON thread_message_files FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());

-- The worker's sweep finds mailboxes due to be read and threads whose
-- snooze has ended (ids and times only); the reading and the waking run
-- inside each workspace as the app role.
CREATE POLICY integrations_worker ON integrations FOR SELECT TO onhold_worker
    USING (disconnected_at IS NULL);
CREATE POLICY threads_worker ON threads FOR SELECT TO onhold_worker
    USING (status = 'snoozed');

-- Whether another live connection, in any workspace, holds a grant for the
-- same account: revoking a Google grant ends them all, so disconnecting one
-- leaves the grant alone while others still use it.
CREATE FUNCTION integration_account_in_use(p_provider text, p_account text, p_except uuid)
    RETURNS boolean
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT EXISTS (
            SELECT 1 FROM integrations
            WHERE provider = p_provider AND account = p_account AND id <> p_except
              AND disconnected_at IS NULL
        )
    $$;
REVOKE ALL ON FUNCTION integration_account_in_use(text, text, uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION integration_account_in_use(text, text, uuid) TO onhold_app;

GRANT SELECT, INSERT, UPDATE ON integrations, threads, thread_messages TO onhold_app;
GRANT SELECT, INSERT ON thread_message_files TO onhold_app;
GRANT SELECT (id, workspace_id, next_poll_at, poll_lease_until, disconnected_at) ON integrations TO onhold_worker;
GRANT SELECT (id, workspace_id, status, snoozed_until) ON threads TO onhold_worker;
