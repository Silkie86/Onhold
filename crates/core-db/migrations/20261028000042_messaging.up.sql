-- M6, fifth part: text messages. A workspace connects its own SMS number
-- (Twilio) and WhatsApp number (Meta's Cloud API); conversations on them
-- are inbox threads, one per number they are with.

-- A workspace's number on one channel. One live per channel.
CREATE TABLE messaging_channels (
    id              uuid PRIMARY KEY,
    workspace_id    uuid NOT NULL REFERENCES workspaces (id),
    -- sms (through Twilio) or whatsapp (through Meta's Cloud API).
    kind            text NOT NULL CHECK (kind IN ('sms', 'whatsapp')),
    provider        text NOT NULL CHECK (provider IN ('twilio', 'meta')),
    -- The number messages come from, in E.164 (+447700900123).
    sender          text NOT NULL CHECK (sender ~ '^\+[1-9][0-9]{6,14}$'),
    -- Twilio's account SID; Meta's phone number id.
    account_id      text NOT NULL CHECK (length(account_id) BETWEEN 1 AND 100),
    -- What the other side sees (WhatsApp's verified name), if known.
    display_name    text CHECK (length(display_name) <= 200),
    -- The credentials, encrypted with the server's keyring and bound to the
    -- row's id; emptied on disconnecting.
    secret          bytea NOT NULL,
    -- The last part of the address the provider sends messages to. Not a
    -- credential: every request is checked against the provider's signature.
    hook_token      text NOT NULL UNIQUE CHECK (length(hook_token) BETWEEN 20 AND 100),
    -- active, or failing (sending refused for the account itself).
    status          text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'failing')),
    last_error      text CHECK (length(last_error) <= 1000),
    last_error_at   timestamptz,
    connected_by    uuid REFERENCES users (id),
    connected_at    timestamptz NOT NULL DEFAULT now(),
    disconnected_at timestamptz,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    version         bigint NOT NULL DEFAULT 1,
    UNIQUE (workspace_id, id),
    CHECK ((kind = 'sms') = (provider = 'twilio'))
);
CREATE UNIQUE INDEX messaging_channels_live ON messaging_channels (workspace_id, kind)
    WHERE disconnected_at IS NULL;
CREATE TRIGGER messaging_channels_updated_at BEFORE UPDATE ON messaging_channels
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER messaging_channels_version BEFORE UPDATE ON messaging_channels
    FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Numbers that asked for no more messages on a channel (STOP), or asked
-- again for them (START). Nothing is sent to an opted-out number.
CREATE TABLE messaging_opt_outs (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    kind         text NOT NULL CHECK (kind IN ('sms', 'whatsapp')),
    phone        text NOT NULL CHECK (phone ~ '^\+[1-9][0-9]{6,14}$'),
    opted_out    boolean NOT NULL,
    changed_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, kind, phone)
);

-- Threads on a number: the channel, and (as `external_id`) the other
-- side's number. Messages on it carry the provider's message id.
ALTER TABLE threads DROP CONSTRAINT threads_channel_check;
ALTER TABLE threads ADD CONSTRAINT threads_channel_check CHECK (channel IN ('email', 'sms', 'whatsapp'));
ALTER TABLE threads ADD COLUMN messaging_channel_id uuid;
ALTER TABLE threads ADD CONSTRAINT threads_messaging_channel_fk
    FOREIGN KEY (workspace_id, messaging_channel_id) REFERENCES messaging_channels (workspace_id, id);
ALTER TABLE threads ADD CONSTRAINT threads_messaging_check
    CHECK ((channel = 'email') = (messaging_channel_id IS NULL));
CREATE UNIQUE INDEX threads_messaging ON threads (messaging_channel_id, external_id)
    WHERE messaging_channel_id IS NOT NULL;

ALTER TABLE thread_messages ADD COLUMN messaging_channel_id uuid;
ALTER TABLE thread_messages ADD CONSTRAINT thread_messages_messaging_channel_fk
    FOREIGN KEY (workspace_id, messaging_channel_id) REFERENCES messaging_channels (workspace_id, id);
CREATE UNIQUE INDEX thread_messages_messaging ON thread_messages (messaging_channel_id, external_id)
    WHERE messaging_channel_id IS NOT NULL AND external_id IS NOT NULL;

ALTER TABLE messaging_channels ENABLE ROW LEVEL SECURITY;
ALTER TABLE messaging_opt_outs ENABLE ROW LEVEL SECURITY;
CREATE POLICY messaging_channels_tenant ON messaging_channels FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY messaging_opt_outs_tenant ON messaging_opt_outs FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());

-- The channel a provider's request is for, by the address it was sent to
-- (before the workspace is known). Live channels only.
CREATE FUNCTION messaging_channel_for_hook(p_token text)
    RETURNS TABLE (workspace_id uuid, id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT c.workspace_id, c.id FROM messaging_channels c
        WHERE c.hook_token = p_token AND c.disconnected_at IS NULL
    $$;
REVOKE ALL ON FUNCTION messaging_channel_for_hook(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION messaging_channel_for_hook(text) TO onhold_app;

GRANT SELECT, INSERT, UPDATE ON messaging_channels, messaging_opt_outs TO onhold_app;

-- Which ready-made follow-up a rule was installed from (`quote_chase`,
-- `invoice_overdue/7`…), so switching the follow-up off and on finds it.
ALTER TABLE rules ADD COLUMN preset text;
CREATE UNIQUE INDEX rules_preset ON rules (workspace_id, preset)
    WHERE preset IS NOT NULL AND deleted_at IS NULL;
