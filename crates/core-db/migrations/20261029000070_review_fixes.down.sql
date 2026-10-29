-- Reverts the fixes from the whole-app review.

DROP FUNCTION stripe_event_workspace(text, uuid, text);
