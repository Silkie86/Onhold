-- Fixes from the whole-app review.

-- Every Checkout session OnHold made, for which workspace and invoice and
-- on which account. An invoice keeps only its current session, but a
-- session it no longer keeps may still be paid (Stripe could not close it
-- in time), and only a session made here is a payment of the workspace's:
-- whoever holds the account can make sessions of their own on it.
CREATE TABLE stripe_checkout_sessions (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    id           text NOT NULL CHECK (length(id) <= 200),
    account_id   text NOT NULL CHECK (length(account_id) <= 100),
    invoice_id   uuid NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, id),
    FOREIGN KEY (workspace_id, invoice_id) REFERENCES invoices (workspace_id, id)
);
CREATE INDEX stripe_checkout_sessions_invoice ON stripe_checkout_sessions (workspace_id, invoice_id);
INSERT INTO stripe_checkout_sessions (workspace_id, id, account_id, invoice_id)
    SELECT workspace_id, stripe_session_id, stripe_session_account, id FROM invoices
    WHERE stripe_session_id IS NOT NULL AND stripe_session_account IS NOT NULL;

ALTER TABLE stripe_checkout_sessions ENABLE ROW LEVEL SECURITY;
CREATE POLICY stripe_checkout_sessions_tenant ON stripe_checkout_sessions FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
GRANT SELECT, INSERT ON stripe_checkout_sessions TO onhold_app;

-- Checkout events still waiting, by the payment they are for, so a refund
-- finds its payment's event without reading every event kept.
CREATE INDEX stripe_events_pending_checkout_intent
    ON stripe_events (workspace_id, (payload #>> '{data,object,payment_intent}'))
    WHERE processed_at IS NULL AND type LIKE 'checkout.session.%';

-- Which workspace a Stripe event belongs to. An account can move from one
-- workspace to another, and Stripe keeps sending events about money that
-- moved while it was with the first: those go where that money belongs.
-- A Checkout session OnHold made counts for the workspace it names, if
-- that workspace made it on this account; a refund goes with the payment
-- it refunds (or the payment's own event, still waiting), so long as that
-- workspace has had the account. Anything else goes to the workspace the
-- account is connected to, else the one it was last connected to.
CREATE FUNCTION stripe_event_workspace(p_account_id text, p_workspace_id uuid,
                                       p_session_id text, p_payment_intent text)
    RETURNS uuid
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT COALESCE(
            (SELECT a.workspace_id FROM stripe_accounts a
             JOIN stripe_checkout_sessions s
               ON s.workspace_id = a.workspace_id AND s.account_id = a.account_id
             WHERE a.account_id = p_account_id AND a.workspace_id = p_workspace_id
               AND s.id = p_session_id),
            (SELECT p.workspace_id FROM payments p
             JOIN stripe_accounts a ON a.workspace_id = p.workspace_id AND a.account_id = p_account_id
             WHERE p.stripe_payment_intent = p_payment_intent AND p.kind = 'payment'
             ORDER BY p.created_at DESC LIMIT 1),
            (SELECT e.workspace_id FROM stripe_events e
             JOIN stripe_accounts a ON a.workspace_id = e.workspace_id AND a.account_id = p_account_id
             WHERE e.processed_at IS NULL AND e.type LIKE 'checkout.session.%'
               AND e.payload #>> '{data,object,payment_intent}' = p_payment_intent
             ORDER BY e.received_at DESC LIMIT 1),
            stripe_account_workspace(p_account_id))
    $$;

REVOKE ALL ON FUNCTION stripe_event_workspace(text, uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION stripe_event_workspace(text, uuid, text, text) TO onhold_app;
