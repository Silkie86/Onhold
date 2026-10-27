-- M5, second part: Stripe Connect. Each workspace connects its own Stripe
-- account (Connect Standard), so money settles to the client directly.
-- Payers pay an invoice through a Stripe Checkout session made on that
-- account; Stripe's webhooks record the payment (and any refund) here.

CREATE TABLE stripe_accounts (
    workspace_id      uuid PRIMARY KEY REFERENCES workspaces (id),
    account_id        text NOT NULL UNIQUE CHECK (account_id ~ '^acct_[A-Za-z0-9]{1,64}$'),
    livemode          boolean NOT NULL DEFAULT false,
    charges_enabled   boolean NOT NULL DEFAULT false,
    details_submitted boolean NOT NULL DEFAULT false,
    default_currency  char(3) CHECK (default_currency ~ '^[A-Z]{3}$'),
    display_name      text CHECK (length(display_name) <= 200),
    connected_at      timestamptz NOT NULL DEFAULT now(),
    connected_by      uuid REFERENCES users (id),
    -- Set when the client disconnects (here or from Stripe's side).
    disconnected_at   timestamptz,
    updated_at        timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER stripe_accounts_updated_at BEFORE UPDATE ON stripe_accounts
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- Which workspace a Stripe account belongs to, for webhooks (which arrive
-- signed by Stripe, not by a signed-in person).
CREATE FUNCTION stripe_account_workspace(p_account_id text)
    RETURNS uuid
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT workspace_id FROM stripe_accounts WHERE account_id = p_account_id
    $$;

-- Every event Stripe sent, once: Stripe retries, and may send one twice.
CREATE TABLE stripe_events (
    id           text NOT NULL CHECK (id ~ '^evt_[A-Za-z0-9]{1,128}$'),
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    type         text NOT NULL CHECK (length(type) <= 100),
    payload      jsonb NOT NULL,
    received_at  timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz,
    outcome      text CHECK (length(outcome) <= 500),
    PRIMARY KEY (workspace_id, id)
);

-- The payer's current Checkout session for an invoice: reused while it is
-- open and for the same amount, replaced otherwise.
ALTER TABLE invoices
    ADD COLUMN stripe_session_id         text CHECK (length(stripe_session_id) <= 200),
    ADD COLUMN stripe_session_url        text CHECK (length(stripe_session_url) <= 2000),
    ADD COLUMN stripe_session_amount     bigint,
    ADD COLUMN stripe_session_expires_at timestamptz;

-- A payment made through Stripe, by its PaymentIntent, so it is recorded once.
ALTER TABLE payments ADD COLUMN stripe_payment_intent text CHECK (length(stripe_payment_intent) <= 200);
CREATE UNIQUE INDEX payments_stripe ON payments (workspace_id, stripe_payment_intent)
    WHERE stripe_payment_intent IS NOT NULL AND kind = 'payment';

ALTER TABLE stripe_accounts ENABLE ROW LEVEL SECURITY;
ALTER TABLE stripe_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY stripe_accounts_tenant ON stripe_accounts FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY stripe_events_tenant ON stripe_events FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());

GRANT SELECT, INSERT, UPDATE ON stripe_accounts, stripe_events TO onhold_app;
REVOKE ALL ON FUNCTION stripe_account_workspace(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION stripe_account_workspace(text) TO onhold_app;
