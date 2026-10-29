-- Fixes from the whole-app review.

-- Which workspace a Stripe event belongs to. An account can move from one
-- workspace to another, and Stripe keeps sending events about money that
-- moved while it was with the first: those go where that money belongs.
-- A Checkout session names its workspace in its metadata, and a refund the
-- payment it refunds (or the payment's own event, still waiting); either
-- counts only for a workspace that has had the account. Anything else goes
-- to the workspace the account is connected to, else the one it was last
-- connected to.
CREATE FUNCTION stripe_event_workspace(p_account_id text, p_workspace_id uuid, p_payment_intent text)
    RETURNS uuid
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT COALESCE(
            (SELECT a.workspace_id FROM stripe_accounts a
             WHERE a.account_id = p_account_id AND a.workspace_id = p_workspace_id),
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

REVOKE ALL ON FUNCTION stripe_event_workspace(text, uuid, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION stripe_event_workspace(text, uuid, text) TO onhold_app;
