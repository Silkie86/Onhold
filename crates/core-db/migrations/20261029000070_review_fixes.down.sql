-- Reverts the fixes from the whole-app review.

DROP FUNCTION stripe_event_workspace(text, uuid, text, text);
DROP INDEX stripe_events_pending_checkout_intent;
DROP TABLE stripe_checkout_sessions;
