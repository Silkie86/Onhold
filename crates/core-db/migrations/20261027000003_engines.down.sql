DROP FUNCTION IF EXISTS admin_job_summary();
DROP TRIGGER IF EXISTS outbox_notify ON outbox;

-- Policies on tables that M1 created and this migration leaves in place.
DROP POLICY IF EXISTS outbox_events ON outbox;
DROP POLICY IF EXISTS outbox_worker ON outbox;
DROP POLICY IF EXISTS workspaces_worker ON workspaces;
DROP POLICY IF EXISTS idempotency_keys_worker ON idempotency_keys;

DROP TABLE IF EXISTS outbox_pruned;
DROP TABLE IF EXISTS outbound_messages;
DROP TABLE IF EXISTS webhook_deliveries;
DROP TABLE IF EXISTS webhook_endpoints;
DROP TABLE IF EXISTS rule_fires;
DROP TABLE IF EXISTS rule_runs;
DROP TABLE IF EXISTS rules;
DROP FUNCTION IF EXISTS rules_bump_version();
DROP TABLE IF EXISTS templates;
DROP TABLE IF EXISTS things;
DROP TABLE IF EXISTS pipeline_stages;
DROP TABLE IF EXISTS pipelines;
DROP TABLE IF EXISTS custom_field_defs;
DROP TABLE IF EXISTS job_runs;
DROP TABLE IF EXISTS jobs;
DROP TYPE IF EXISTS job_status;
DROP FUNCTION IF EXISTS notify_channel();

REVOKE SELECT (id, workspace_id, topic, payload, created_at) ON outbox FROM onhold_app;
DROP INDEX IF EXISTS outbox_workspace_seq;
ALTER TABLE outbox DROP COLUMN IF EXISTS secret, DROP COLUMN IF EXISTS seq;
DROP SEQUENCE IF EXISTS outbox_seq;

ALTER TABLE audit_log DROP COLUMN IF EXISTS actor_rule_id;
ALTER TABLE workspaces DROP COLUMN IF EXISTS automations_paused;

DELETE FROM role_permissions WHERE permission IN (
    'config:write', 'thing:read', 'thing:write', 'event:read', 'webhook:read', 'webhook:write',
    'job:read', 'job:write');

-- Like onhold_app, the worker role is cluster-wide and stays; this database's
-- grants go.
REVOKE ALL ON outbox, workspaces, idempotency_keys FROM onhold_worker;
REVOKE USAGE ON SCHEMA public FROM onhold_worker;
