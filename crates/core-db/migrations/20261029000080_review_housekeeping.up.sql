-- Fixes from the whole-app review: answers to pitches found without
-- reading all of a workspace's mail, jobs claimed fairly between
-- workspaces, and old rows the worker now clears out.

-- An answer to a pitch is found by who sent it: the sender, or the
-- address replies go to.
CREATE INDEX thread_messages_inbound_from ON thread_messages (workspace_id, from_address, sent_at)
    WHERE direction = 'inbound';
CREATE INDEX thread_messages_inbound_reply_to ON thread_messages (workspace_id, reply_to, sent_at)
    WHERE direction = 'inbound' AND reply_to IS NOT NULL;

-- Ready jobs take turns between workspaces: each workspace's in its own
-- order.
CREATE INDEX jobs_ready_workspace ON jobs (workspace_id, priority DESC, run_at, id)
    WHERE status = 'queued';

-- Housekeeping -----------------------------------------------------------------

-- What the worker clears out is found by when it ended, not by reading
-- whole tables.
CREATE INDEX jobs_succeeded_finished ON jobs (finished_at) WHERE status = 'succeeded';
CREATE INDEX jobs_given_up_finished ON jobs ((coalesce(finished_at, updated_at)))
    WHERE status IN ('dead', 'cancelled');
CREATE INDEX webhook_deliveries_created ON webhook_deliveries (created_at);
CREATE INDEX rule_runs_skipped_created ON rule_runs (created_at) WHERE outcome = 'skipped';
CREATE INDEX stripe_events_payload_kept ON stripe_events (processed_at)
    WHERE payload <> '{}'::jsonb;
CREATE INDEX sessions_expires ON sessions (expires_at);
CREATE INDEX sessions_revoked ON sessions (revoked_at) WHERE revoked_at IS NOT NULL;
CREATE INDEX magic_links_expires ON magic_links (expires_at);
CREATE INDEX magic_links_used ON magic_links (used_at) WHERE used_at IS NOT NULL;
CREATE INDEX invitations_expires ON invitations (expires_at);
CREATE INDEX invitations_accepted ON invitations (accepted_at) WHERE accepted_at IS NOT NULL;
CREATE INDEX invitations_revoked ON invitations (revoked_at) WHERE revoked_at IS NOT NULL;
CREATE INDEX calendar_connect_links_expires ON calendar_connect_links (expires_at);
CREATE INDEX calendar_connect_links_revoked ON calendar_connect_links (revoked_at)
    WHERE revoked_at IS NOT NULL;
CREATE INDEX talent_calendars_link ON talent_calendars (workspace_id, link_id)
    WHERE link_id IS NOT NULL;

-- Webhook deliveries and the rule runs that did nothing are logs, cleared
-- after a while.
CREATE POLICY webhook_deliveries_worker ON webhook_deliveries FOR SELECT TO onhold_worker
    USING (true);
CREATE POLICY webhook_deliveries_worker_delete ON webhook_deliveries FOR DELETE TO onhold_worker
    USING (true);
CREATE POLICY rule_runs_worker ON rule_runs FOR SELECT TO onhold_worker
    USING (outcome = 'skipped');
CREATE POLICY rule_runs_worker_delete ON rule_runs FOR DELETE TO onhold_worker
    USING (outcome = 'skipped');
GRANT SELECT (created_at), DELETE ON webhook_deliveries TO onhold_worker;
GRANT SELECT (outcome, created_at), DELETE ON rule_runs TO onhold_worker;

-- Stripe events: what Stripe sent is kept a while after it was handled,
-- then only that it came, its type and what came of it. The worker never
-- reads the payloads, so it goes through a function.
CREATE FUNCTION stripe_events_forget_before(p_before timestamptz)
    RETURNS bigint
    LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        WITH done AS (
            UPDATE stripe_events SET payload = '{}'::jsonb
            WHERE processed_at < p_before AND payload <> '{}'::jsonb
            RETURNING 1
        )
        SELECT count(*) FROM done
    $$;

-- Sign-ins, invitations and calendar links that ended (expired, used up,
-- revoked or accepted) before `p_before`. A calendar link a connected
-- calendar came through stays.
CREATE FUNCTION sign_ins_forget_before(p_before timestamptz)
    RETURNS bigint
    LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        WITH s AS (
            DELETE FROM sessions WHERE expires_at < p_before OR revoked_at < p_before RETURNING 1
        ), m AS (
            DELETE FROM magic_links WHERE expires_at < p_before OR used_at < p_before RETURNING 1
        ), i AS (
            DELETE FROM invitations
            WHERE expires_at < p_before OR accepted_at < p_before OR revoked_at < p_before
            RETURNING 1
        ), c AS (
            DELETE FROM calendar_connect_links l
            WHERE (l.expires_at < p_before OR l.revoked_at < p_before)
              AND NOT EXISTS (SELECT 1 FROM talent_calendars tc
                              WHERE tc.workspace_id = l.workspace_id AND tc.link_id = l.id)
            RETURNING 1
        )
        SELECT (SELECT count(*) FROM s) + (SELECT count(*) FROM m)
             + (SELECT count(*) FROM i) + (SELECT count(*) FROM c)
    $$;

REVOKE ALL ON FUNCTION stripe_events_forget_before(timestamptz),
    sign_ins_forget_before(timestamptz) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION stripe_events_forget_before(timestamptz),
    sign_ins_forget_before(timestamptz) TO onhold_worker;
