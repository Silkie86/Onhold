-- Pitching, part 3c: answers and pitches spotted in the mailbox. An action
-- notes when the organisation answered it, with the message, as soon as an
-- answer from them arrives in the inbox; a pitch emailed from the member's
-- own mailbox outside OnHold can be logged from its copy in Sent
-- (`spotted`), or dismissed as not a pitch.
ALTER TABLE pitch_log DROP CONSTRAINT pitch_log_source_check;
ALTER TABLE pitch_log DROP CONSTRAINT pitch_log_sent_message;
ALTER TABLE pitch_log ADD CONSTRAINT pitch_log_source_check CHECK (source IN ('manual', 'sent', 'spotted'));
ALTER TABLE pitch_log ADD CONSTRAINT pitch_log_sent_message CHECK ((source = 'manual') = (message_id IS NULL));
CREATE UNIQUE INDEX pitch_log_one_per_message ON pitch_log (workspace_id, message_id)
    WHERE message_id IS NOT NULL AND deleted_at IS NULL;
DROP INDEX pitch_log_message;

ALTER TABLE pitch_log
    ADD COLUMN answered_at timestamptz,
    ADD COLUMN answer_message_id uuid,
    ADD FOREIGN KEY (workspace_id, answer_message_id) REFERENCES thread_messages (workspace_id, id),
    ADD CONSTRAINT pitch_log_answer CHECK ((answered_at IS NULL) = (answer_message_id IS NULL)),
    ADD CONSTRAINT pitch_log_answer_kind CHECK (answered_at IS NULL OR kind = 'action');
CREATE INDEX pitch_log_unanswered ON pitch_log (workspace_id, organisation_id, happened_at DESC)
    WHERE kind = 'action' AND answered_at IS NULL AND deleted_at IS NULL;

-- Mail in Sent that is not a pitch, so it is not offered again.
CREATE TABLE pitch_not_pitches (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    message_id   uuid NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    PRIMARY KEY (workspace_id, message_id),
    FOREIGN KEY (workspace_id, message_id) REFERENCES thread_messages (workspace_id, id)
);
ALTER TABLE pitch_not_pitches ENABLE ROW LEVEL SECURITY;
CREATE POLICY pitch_not_pitches_tenant ON pitch_not_pitches FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
GRANT SELECT, INSERT ON pitch_not_pitches TO onhold_app;
