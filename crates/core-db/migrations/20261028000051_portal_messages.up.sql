-- Messages between a talent and their agency, through the portal. Each
-- talent record has one conversation with the person its portal is open
-- for (someone invited later starts afresh), which sits in the agency's
-- inbox like an email or text thread; the talent reads and writes it from
-- their portal page.

ALTER TABLE threads DROP CONSTRAINT threads_channel_check;
ALTER TABLE threads ADD CONSTRAINT threads_channel_check
    CHECK (channel IN ('email', 'sms', 'whatsapp', 'portal'));
ALTER TABLE threads DROP CONSTRAINT threads_messaging_check;
ALTER TABLE threads ADD CONSTRAINT threads_messaging_check
    CHECK ((channel IN ('email', 'portal')) = (messaging_channel_id IS NULL));

ALTER TABLE threads ADD COLUMN talent_id uuid;
ALTER TABLE threads ADD COLUMN portal_user_id uuid REFERENCES users (id);
-- When the talent last read the conversation, for their unread count.
ALTER TABLE threads ADD COLUMN portal_read_at timestamptz;
ALTER TABLE threads ADD CONSTRAINT threads_talent_fk
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id);
ALTER TABLE threads ADD CONSTRAINT threads_portal_check
    CHECK ((channel = 'portal') = (talent_id IS NOT NULL)
           AND (channel = 'portal') = (portal_user_id IS NOT NULL)
           AND (portal_read_at IS NULL OR channel = 'portal'));
CREATE UNIQUE INDEX threads_portal ON threads (workspace_id, talent_id, portal_user_id)
    WHERE channel = 'portal';
