-- Reverts Stripe Connect.

DROP INDEX payments_stripe;
ALTER TABLE payments DROP COLUMN stripe_payment_intent;
ALTER TABLE invoices
    DROP COLUMN stripe_session_id,
    DROP COLUMN stripe_session_url,
    DROP COLUMN stripe_session_amount,
    DROP COLUMN stripe_session_expires_at,
    DROP COLUMN stripe_session_account;
DROP TABLE stripe_events;
DROP FUNCTION stripe_account_live_workspace(text);
DROP FUNCTION stripe_account_workspace(text);
DROP TABLE stripe_accounts;
