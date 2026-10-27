-- M5: documents and money. Files are stored once (in the database, a
-- directory or an S3-compatible bucket) and described here; documents are
-- generated from templates or uploaded, versioned, and can be signed by
-- secret link. Invoices and credit notes carry their lines, tax and
-- currency, are numbered without gaps per issuer when issued, and are
-- frozen from then on. Payments are allocated to invoices; settlements
-- work out what the talent is owed after commission. Exchange rates are
-- kept by day, and every converted amount stores the rate it used.
--
-- Same conventions as before: UUIDv7 ids, versions, soft deletes, `custom`
-- and `tags`, links that carry the workspace in their foreign keys,
-- row-level security on every tenant table.

-- Permissions -----------------------------------------------------------------

INSERT INTO role_permissions (workspace_id, role, permission)
SELECT w.id, r.role::member_role, r.permission
FROM workspaces w
CROSS JOIN (VALUES
    ('owner', 'document:read'), ('admin', 'document:read'), ('agent', 'document:read'),
    ('finance', 'document:read'), ('viewer', 'document:read'),
    ('owner', 'document:write'), ('admin', 'document:write'), ('agent', 'document:write'),
    ('finance', 'document:write'),
    ('owner', 'invoice:read'), ('admin', 'invoice:read'), ('agent', 'invoice:read'),
    ('finance', 'invoice:read'), ('viewer', 'invoice:read'),
    ('owner', 'invoice:write'), ('admin', 'invoice:write'), ('agent', 'invoice:write'),
    ('finance', 'invoice:write'),
    ('owner', 'payment:write'), ('admin', 'payment:write'), ('finance', 'payment:write'),
    ('owner', 'report:read'), ('admin', 'report:read'), ('finance', 'report:read'),
    ('owner', 'finance:config'), ('admin', 'finance:config'), ('finance', 'finance:config')
) AS r (role, permission)
ON CONFLICT DO NOTHING;

-- Records rules, tasks and imports can now be about ------------------------------

ALTER TABLE templates DROP CONSTRAINT templates_kind_check;
ALTER TABLE templates ADD CONSTRAINT templates_kind_check
    CHECK (kind IN ('email', 'sms', 'whatsapp', 'notification', 'partial', 'document'));

ALTER TABLE tasks DROP CONSTRAINT tasks_related_entity_check;
ALTER TABLE tasks ADD CONSTRAINT tasks_related_entity_check
    CHECK (related_entity IN ('talent', 'organisation', 'contact', 'enquiry', 'booking', 'quote',
                              'invoice', 'document'));

ALTER TABLE imports DROP CONSTRAINT imports_entity_check;
ALTER TABLE imports ADD CONSTRAINT imports_entity_check
    CHECK (entity IN ('talent', 'organisation', 'contact', 'invoice'));

-- Who invoices for talent: their legal address, tax number and bank details.
ALTER TABLE talent ADD COLUMN billing jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(billing) = 'object');
-- A rate pinned for converting this booking's money into the base currency.
ALTER TABLE bookings ADD COLUMN fx_rate numeric(20, 10) CHECK (fx_rate > 0);

-- Files ------------------------------------------------------------------------

-- A stored file: an upload, a generated PDF, a signed copy. The bytes live
-- in `storage` under `storage_key`; the text read from a PDF is kept for
-- search. A file nothing uses yet (`attached_at` empty) is deleted after a
-- week.
CREATE TABLE files (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    filename     text NOT NULL CHECK (length(btrim(filename)) BETWEEN 1 AND 255),
    content_type text NOT NULL CHECK (length(content_type) BETWEEN 1 AND 100),
    size_bytes   bigint NOT NULL CHECK (size_bytes >= 0),
    sha256       bytea NOT NULL CHECK (length(sha256) = 32),
    storage      text NOT NULL CHECK (storage IN ('database', 'directory', 's3')),
    storage_key  text NOT NULL CHECK (length(storage_key) BETWEEN 1 AND 500),
    -- Reading the text out of a PDF: pending, done, failed or skipped (not a PDF).
    read_state   text NOT NULL DEFAULT 'skipped' CHECK (read_state IN ('pending', 'done', 'failed', 'skipped')),
    read_error   text CHECK (length(read_error) <= 1000),
    page_count   integer CHECK (page_count >= 0),
    -- Pages that are scans with no text (OCR found nothing or was not installed).
    needs_ocr    boolean NOT NULL DEFAULT false,
    text_content text CHECK (length(text_content) <= 2000000),
    text_search  tsvector GENERATED ALWAYS AS (
        to_tsvector('simple', left(coalesce(text_content, ''), 500000))) STORED,
    attached_at  timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    deleted_at   timestamptz,
    UNIQUE (workspace_id, id)
);
CREATE INDEX files_unattached ON files (created_at) WHERE attached_at IS NULL AND deleted_at IS NULL;
CREATE INDEX files_text ON files USING gin (text_search);

-- The bytes of files kept in the database (the default storage).
CREATE TABLE file_blobs (
    file_id      uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    data         bytea NOT NULL,
    FOREIGN KEY (workspace_id, file_id) REFERENCES files (workspace_id, id) ON DELETE CASCADE
);

-- Documents ----------------------------------------------------------------------

CREATE TABLE documents (
    id               uuid PRIMARY KEY,
    workspace_id     uuid NOT NULL REFERENCES workspaces (id),
    kind             text NOT NULL CHECK (kind IN (
        'contract', 'confirmation', 'advance_sheet', 'invoice', 'credit_note', 'quote', 'statement',
        'rider', 'tech_spec', 'stage_plot', 'epk', 'insurance', 'passport', 'visa', 'receipt',
        'purchase_order', 'misc')),
    title            text NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    -- The record it belongs to.
    related_entity   text CHECK (related_entity IN ('talent', 'organisation', 'contact', 'enquiry', 'booking', 'invoice')),
    related_id       uuid,
    source           text NOT NULL CHECK (source IN ('generated', 'uploaded', 'imported')),
    -- The template a generated document was made from.
    template_key     text CHECK (length(template_key) <= 100),
    current_version  integer NOT NULL DEFAULT 0 CHECK (current_version >= 0),
    -- The date on the paper (a contract's date, an old invoice's issue date).
    document_date    date,
    -- When an insurance certificate or passport runs out.
    expires_on       date,
    signature_status text NOT NULL DEFAULT 'none' CHECK (signature_status IN ('none', 'awaiting', 'signed', 'declined')),
    -- The round of signatures the signed copy was sealed from.
    signed_round_id  uuid,
    notes            text CHECK (length(notes) <= 5000),
    tags             text[] NOT NULL DEFAULT '{}',
    custom           jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom) = 'object'),
    version          bigint NOT NULL DEFAULT 1,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    created_by       uuid REFERENCES users (id),
    updated_by       uuid REFERENCES users (id),
    deleted_at       timestamptz,
    search_text      text GENERATED ALWAYS AS (lower(title || ' ' || search_join(tags))) STORED,
    UNIQUE (workspace_id, id),
    CHECK ((related_entity IS NULL) = (related_id IS NULL))
);
CREATE INDEX documents_workspace ON documents (workspace_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX documents_related ON documents (workspace_id, related_id) WHERE deleted_at IS NULL;
CREATE INDEX documents_expiry ON documents (workspace_id, expires_on) WHERE expires_on IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX documents_search ON documents USING gin (search_text gin_trgm_ops);
CREATE INDEX documents_custom ON documents USING gin (custom jsonb_path_ops);
CREATE TRIGGER documents_updated_at BEFORE UPDATE ON documents FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER documents_version BEFORE UPDATE ON documents FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Each version of a document. A generated one keeps the HTML it was printed
-- from; a signed copy is a version of its own.
CREATE TABLE document_versions (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    document_id  uuid NOT NULL,
    number       integer NOT NULL CHECK (number >= 1),
    purpose      text NOT NULL DEFAULT 'original' CHECK (purpose IN ('original', 'signed')),
    status       text NOT NULL DEFAULT 'ready' CHECK (status IN ('rendering', 'ready', 'failed')),
    file_id      uuid,
    html         text CHECK (length(html) <= 2000000),
    error        text CHECK (length(error) <= 1000),
    created_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    UNIQUE (document_id, number),
    UNIQUE (workspace_id, id),
    CHECK (status <> 'ready' OR file_id IS NOT NULL),
    FOREIGN KEY (workspace_id, document_id) REFERENCES documents (workspace_id, id),
    FOREIGN KEY (workspace_id, file_id) REFERENCES files (workspace_id, id)
);
CREATE INDEX document_versions_file ON document_versions (file_id);

-- A request for one person to sign one version, by secret link.
CREATE TABLE signature_requests (
    id                uuid PRIMARY KEY,
    workspace_id      uuid NOT NULL REFERENCES workspaces (id),
    document_id       uuid NOT NULL,
    version_number    integer NOT NULL,
    -- One request for signatures: its signers sign together.
    round_id          uuid NOT NULL,
    position          integer NOT NULL DEFAULT 0,
    signer_name       text NOT NULL CHECK (length(btrim(signer_name)) BETWEEN 1 AND 200),
    signer_email      text NOT NULL CHECK (length(signer_email) BETWEEN 3 AND 320),
    status            text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'signed', 'declined', 'cancelled')),
    token_hash        bytea NOT NULL UNIQUE,
    sent_at           timestamptz NOT NULL DEFAULT now(),
    viewed_at         timestamptz,
    decided_at        timestamptz,
    -- The name typed as the signature, and where it came from.
    signed_name       text CHECK (length(signed_name) <= 200),
    signed_ip         inet,
    signed_user_agent text CHECK (length(signed_user_agent) <= 500),
    -- The SHA-256 of the PDF as it was shown and signed.
    pdf_sha256        bytea CHECK (length(pdf_sha256) = 32),
    decline_reason    text CHECK (length(decline_reason) <= 2000),
    version           bigint NOT NULL DEFAULT 1,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    created_by        uuid REFERENCES users (id),
    UNIQUE (workspace_id, id),
    CHECK (status <> 'signed' OR (signed_name IS NOT NULL AND pdf_sha256 IS NOT NULL AND decided_at IS NOT NULL)),
    FOREIGN KEY (workspace_id, document_id) REFERENCES documents (workspace_id, id),
    FOREIGN KEY (document_id, version_number) REFERENCES document_versions (document_id, number)
);
CREATE INDEX signature_requests_document ON signature_requests (workspace_id, document_id);
CREATE TRIGGER signature_requests_updated_at BEFORE UPDATE ON signature_requests FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER signature_requests_version BEFORE UPDATE ON signature_requests FOR EACH ROW EXECUTE FUNCTION bump_version();

-- A signature, once given, never changes.
CREATE FUNCTION signature_requests_freeze_signed() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF OLD.status = 'signed' AND (
        NEW.status IS DISTINCT FROM OLD.status OR NEW.signed_name IS DISTINCT FROM OLD.signed_name
        OR NEW.signed_ip IS DISTINCT FROM OLD.signed_ip OR NEW.pdf_sha256 IS DISTINCT FROM OLD.pdf_sha256
        OR NEW.decided_at IS DISTINCT FROM OLD.decided_at OR NEW.signer_email IS DISTINCT FROM OLD.signer_email
    ) THEN
        RAISE EXCEPTION 'a signature cannot change' USING ERRCODE = 'check_violation',
            CONSTRAINT = 'signature_requests_signed_frozen';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER signature_requests_freeze_signed BEFORE UPDATE ON signature_requests
    FOR EACH ROW EXECUTE FUNCTION signature_requests_freeze_signed();

-- The signing request behind a signer's link.
CREATE FUNCTION signature_public_find(p_token_hash bytea)
    RETURNS TABLE (id uuid, workspace_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT s.id, s.workspace_id FROM signature_requests s
        WHERE s.token_hash = p_token_hash AND s.status <> 'cancelled'
    $$;

-- Tax --------------------------------------------------------------------------------

CREATE TABLE tax_codes (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    -- Hundredths of a percent: 2000 is 20%.
    rate_bps     integer NOT NULL CHECK (rate_bps BETWEEN 0 AND 10000),
    kind         text NOT NULL DEFAULT 'standard'
                 CHECK (kind IN ('standard', 'reduced', 'zero', 'exempt', 'reverse_charge', 'out_of_scope')),
    -- Wording printed on an invoice that uses it ("Reverse charge: …").
    note         text CHECK (length(note) <= 500),
    is_default   boolean NOT NULL DEFAULT false,
    position     integer NOT NULL DEFAULT 0,
    archived_at  timestamptz,
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    UNIQUE (workspace_id, id),
    CHECK (kind NOT IN ('zero', 'exempt', 'reverse_charge', 'out_of_scope') OR rate_bps = 0)
);
CREATE UNIQUE INDEX tax_codes_one_default ON tax_codes (workspace_id) WHERE is_default AND archived_at IS NULL;
CREATE TRIGGER tax_codes_updated_at BEFORE UPDATE ON tax_codes FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER tax_codes_version BEFORE UPDATE ON tax_codes FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Finance settings ---------------------------------------------------------------------

-- How the workspace invoices: who it is on paper, how invoices are
-- numbered, its terms and bank details. One row per workspace, made on
-- first use.
CREATE TABLE finance_settings (
    workspace_id       uuid PRIMARY KEY REFERENCES workspaces (id),
    legal_name         text CHECK (length(legal_name) <= 200),
    -- {line1, line2, city, region, postcode, country}
    address            jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(address) = 'object'),
    tax_number         text CHECK (length(tax_number) <= 50),
    company_number     text CHECK (length(company_number) <= 50),
    email              text CHECK (length(email) <= 320),
    phone              text CHECK (length(phone) <= 50),
    invoice_format     text NOT NULL DEFAULT 'INV-{YYYY}-{0000}' CHECK (length(invoice_format) BETWEEN 1 AND 60),
    credit_note_format text NOT NULL DEFAULT 'CN-{YYYY}-{0000}' CHECK (length(credit_note_format) BETWEEN 1 AND 60),
    payment_terms_days integer NOT NULL DEFAULT 14 CHECK (payment_terms_days BETWEEN 0 AND 365),
    -- Who invoices the buyer: the agency (and pays talent out), the talent
    -- (the agency invoices them for commission), or a split (deposit to the
    -- agency, balance from the talent).
    invoicing_model    text NOT NULL DEFAULT 'agency' CHECK (invoicing_model IN ('agency', 'talent', 'split')),
    -- [{currency, label, details}]: printed on invoices in that currency.
    bank_accounts      jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(bank_accounts) = 'array'),
    late_fee_text      text CHECK (length(late_fee_text) <= 1000),
    invoice_footer     text CHECK (length(invoice_footer) <= 2000),
    version            bigint NOT NULL DEFAULT 1,
    updated_at         timestamptz NOT NULL DEFAULT now(),
    updated_by         uuid REFERENCES users (id)
);
CREATE TRIGGER finance_settings_updated_at BEFORE UPDATE ON finance_settings FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER finance_settings_version BEFORE UPDATE ON finance_settings FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Invoice numbers per issuer (the workspace, or one talent) and kind. Taken
-- inside the issuing transaction, so numbers have no gaps.
CREATE TABLE invoice_sequences (
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    -- `workspace`, or the issuing talent's id.
    issuer_key   text NOT NULL CHECK (length(issuer_key) BETWEEN 1 AND 40),
    kind         text NOT NULL CHECK (kind IN ('invoice', 'credit_note')),
    -- The last number used.
    value        bigint NOT NULL DEFAULT 0 CHECK (value >= 0),
    PRIMARY KEY (workspace_id, issuer_key, kind)
);

-- Exchange rates ------------------------------------------------------------------------

-- Reference rates by day: how many units of `currency` one euro buys (the
-- ECB's form). Not tenant data: the same public rates serve everyone.
CREATE TABLE fx_rates (
    rate_date  date NOT NULL,
    currency   char(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    per_eur    numeric(20, 10) NOT NULL CHECK (per_eur > 0),
    source     text NOT NULL DEFAULT 'ecb' CHECK (length(source) BETWEEN 1 AND 20),
    fetched_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (currency, rate_date)
);

-- Invoices -------------------------------------------------------------------------------

CREATE TYPE invoice_status AS ENUM ('draft', 'issued', 'part_paid', 'paid', 'overdue', 'credited', 'void');

CREATE TABLE invoices (
    id                    uuid PRIMARY KEY,
    workspace_id          uuid NOT NULL REFERENCES workspaces (id),
    kind                  text NOT NULL DEFAULT 'invoice' CHECK (kind IN ('invoice', 'credit_note')),
    purpose               text NOT NULL DEFAULT 'other'
                          CHECK (purpose IN ('deposit', 'balance', 'full', 'commission', 'other')),
    status                invoice_status NOT NULL DEFAULT 'draft',
    -- From the issuer's sequence when issued; none for drafts and past invoices.
    number                bigint CHECK (number >= 1),
    -- The number as printed: `INV-2026-0042`, or a past invoice's own.
    reference             text CHECK (length(reference) BETWEEN 1 AND 60),
    -- Brought in from before OnHold: kept as it was, never sent or renumbered.
    historical            boolean NOT NULL DEFAULT false,
    issuer_kind           text NOT NULL DEFAULT 'workspace' CHECK (issuer_kind IN ('workspace', 'talent')),
    issuer_talent_id      uuid,
    -- Who is billed: an organisation, a talent (for commission), or a name only.
    payer_kind            text NOT NULL DEFAULT 'organisation' CHECK (payer_kind IN ('organisation', 'talent', 'other')),
    payer_organisation_id uuid,
    payer_talent_id       uuid,
    payer_contact_id      uuid,
    payer_name            text CHECK (length(payer_name) <= 200),
    -- Issuer and payer details as printed, frozen at issue.
    issuer                jsonb,
    payer                 jsonb,
    booking_id            uuid,
    -- For a credit note: the invoice it reverses.
    credits_invoice_id    uuid,
    currency              char(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    -- [{description, quantity, unit_minor, amount_minor, tax_code_id, tax_rate_bps, talent_id?}]
    line_items            jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(line_items) = 'array'),
    -- [{tax_code_id, name, rate_bps, net_minor, tax_minor, note}]
    tax_lines             jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(tax_lines) = 'array'),
    subtotal_minor        bigint NOT NULL DEFAULT 0,
    tax_minor             bigint NOT NULL DEFAULT 0,
    total_minor           bigint NOT NULL DEFAULT 0 CHECK (total_minor >= 0),
    -- Allocated payments and issued credit notes against this invoice.
    paid_minor            bigint NOT NULL DEFAULT 0 CHECK (paid_minor >= 0),
    -- What a past invoice had been paid before it was imported (it has no
    -- payments here); always counted in `paid_minor`.
    paid_before_minor     bigint NOT NULL DEFAULT 0 CHECK (paid_before_minor >= 0),
    credited_minor        bigint NOT NULL DEFAULT 0 CHECK (credited_minor >= 0),
    issue_date            date,
    due_date              date,
    -- Base currency per unit of `currency` on `fx_date`, and the total in it.
    base_currency         char(3) CHECK (base_currency ~ '^[A-Z]{3}$'),
    fx_rate               numeric(20, 10) CHECK (fx_rate > 0),
    fx_date               date,
    fx_source             text CHECK (fx_source IN ('same', 'ecb', 'booking', 'manual')),
    base_total_minor      bigint,
    notes                 text CHECK (length(notes) <= 5000),
    terms                 text CHECK (length(terms) <= 20000),
    -- The buyer's link; set when first sent.
    token_hash            bytea UNIQUE,
    sent_at               timestamptz,
    sent_to               text CHECK (length(sent_to) <= 320),
    viewed_at             timestamptz,
    -- The locked PDF made on issue.
    document_id           uuid,
    voided_at             timestamptz,
    void_reason           text CHECK (length(void_reason) <= 500),
    tags                  text[] NOT NULL DEFAULT '{}',
    custom                jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(custom) = 'object'),
    version               bigint NOT NULL DEFAULT 1,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    created_by            uuid REFERENCES users (id),
    updated_by            uuid REFERENCES users (id),
    deleted_at            timestamptz,
    search_text           text GENERATED ALWAYS AS (lower(
        coalesce(reference, '') || ' ' || coalesce(payer_name, '') || ' ' || search_join(tags))) STORED,
    UNIQUE (workspace_id, id),
    CHECK ((issuer_kind = 'talent') = (issuer_talent_id IS NOT NULL)),
    CHECK (payer_kind <> 'organisation' OR payer_organisation_id IS NOT NULL),
    CHECK (payer_kind <> 'talent' OR payer_talent_id IS NOT NULL),
    CHECK (payer_kind <> 'other' OR payer_name IS NOT NULL),
    CHECK (status = 'draft' OR (reference IS NOT NULL AND issue_date IS NOT NULL)),
    CHECK (status <> 'draft' OR historical OR number IS NULL),
    CHECK (kind = 'invoice' OR credits_invoice_id IS NOT NULL OR historical),
    CHECK (due_date IS NULL OR issue_date IS NULL OR due_date >= issue_date),
    CHECK ((status = 'void') = (voided_at IS NOT NULL)),
    FOREIGN KEY (workspace_id, issuer_talent_id) REFERENCES talent (workspace_id, id),
    FOREIGN KEY (workspace_id, payer_organisation_id) REFERENCES organisations (workspace_id, id),
    FOREIGN KEY (workspace_id, payer_talent_id) REFERENCES talent (workspace_id, id),
    FOREIGN KEY (workspace_id, payer_contact_id) REFERENCES contacts (workspace_id, id),
    FOREIGN KEY (workspace_id, booking_id) REFERENCES bookings (workspace_id, id),
    FOREIGN KEY (workspace_id, credits_invoice_id) REFERENCES invoices (workspace_id, id),
    FOREIGN KEY (workspace_id, document_id) REFERENCES documents (workspace_id, id)
);
-- A number is used once per issuer and kind, whatever happens to the invoice.
CREATE UNIQUE INDEX invoices_number ON invoices (workspace_id, kind, issuer_kind, coalesce(issuer_talent_id, workspace_id), number)
    WHERE number IS NOT NULL;
CREATE INDEX invoices_workspace ON invoices (workspace_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX invoices_status ON invoices (workspace_id, status) WHERE deleted_at IS NULL;
CREATE INDEX invoices_booking ON invoices (workspace_id, booking_id) WHERE deleted_at IS NULL;
CREATE INDEX invoices_payer ON invoices (workspace_id, payer_organisation_id) WHERE deleted_at IS NULL;
CREATE INDEX invoices_credits ON invoices (workspace_id, credits_invoice_id) WHERE credits_invoice_id IS NOT NULL;
CREATE INDEX invoices_search ON invoices USING gin (search_text gin_trgm_ops);
CREATE INDEX invoices_custom ON invoices USING gin (custom jsonb_path_ops);
-- The worker's sweep: issued invoices passing their due date.
CREATE INDEX invoices_due ON invoices (due_date) WHERE kind = 'invoice' AND status IN ('issued', 'part_paid') AND deleted_at IS NULL;
CREATE TRIGGER invoices_updated_at BEFORE UPDATE ON invoices FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER invoices_version BEFORE UPDATE ON invoices FOR EACH ROW EXECUTE FUNCTION bump_version();

-- An issued invoice never changes what it says; only its payment state,
-- its link and its PDF move on. Mistakes are put right with a credit note.
CREATE FUNCTION invoices_freeze_issued() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF OLD.status <> 'draft' AND (
        NEW.status = 'draft' OR NEW.kind IS DISTINCT FROM OLD.kind
        OR NEW.number IS DISTINCT FROM OLD.number OR NEW.reference IS DISTINCT FROM OLD.reference
        OR NEW.issuer_kind IS DISTINCT FROM OLD.issuer_kind OR NEW.issuer_talent_id IS DISTINCT FROM OLD.issuer_talent_id
        OR NEW.issuer IS DISTINCT FROM OLD.issuer OR NEW.payer IS DISTINCT FROM OLD.payer
        OR NEW.payer_kind IS DISTINCT FROM OLD.payer_kind
        OR NEW.payer_organisation_id IS DISTINCT FROM OLD.payer_organisation_id
        OR NEW.payer_talent_id IS DISTINCT FROM OLD.payer_talent_id
        OR NEW.currency IS DISTINCT FROM OLD.currency OR NEW.line_items IS DISTINCT FROM OLD.line_items
        OR NEW.tax_lines IS DISTINCT FROM OLD.tax_lines OR NEW.subtotal_minor IS DISTINCT FROM OLD.subtotal_minor
        OR NEW.tax_minor IS DISTINCT FROM OLD.tax_minor OR NEW.total_minor IS DISTINCT FROM OLD.total_minor
        OR NEW.issue_date IS DISTINCT FROM OLD.issue_date OR NEW.due_date IS DISTINCT FROM OLD.due_date
        OR NEW.fx_rate IS DISTINCT FROM OLD.fx_rate OR NEW.base_total_minor IS DISTINCT FROM OLD.base_total_minor
        OR NEW.credits_invoice_id IS DISTINCT FROM OLD.credits_invoice_id
        OR NEW.paid_before_minor IS DISTINCT FROM OLD.paid_before_minor
        OR NEW.deleted_at IS DISTINCT FROM OLD.deleted_at
        OR (OLD.status = 'void' AND NEW.status <> 'void')
    ) THEN
        RAISE EXCEPTION 'an issued invoice cannot change' USING ERRCODE = 'check_violation',
            CONSTRAINT = 'invoices_issued_frozen';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER invoices_freeze_issued BEFORE UPDATE ON invoices FOR EACH ROW EXECUTE FUNCTION invoices_freeze_issued();

-- The invoice behind a buyer's link.
CREATE FUNCTION invoice_public_find(p_token_hash bytea)
    RETURNS TABLE (id uuid, workspace_id uuid)
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public
    AS $$
        SELECT i.id, i.workspace_id FROM invoices i
        WHERE i.token_hash = p_token_hash AND i.deleted_at IS NULL AND i.status <> 'draft'
    $$;

-- Payments -------------------------------------------------------------------------------

CREATE TABLE payments (
    id              uuid PRIMARY KEY,
    workspace_id    uuid NOT NULL REFERENCES workspaces (id),
    -- Money in (a payment), or out (a refund, a payout to talent).
    kind            text NOT NULL DEFAULT 'payment' CHECK (kind IN ('payment', 'refund', 'payout')),
    amount_minor    bigint NOT NULL CHECK (amount_minor > 0),
    currency        char(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    method          text NOT NULL DEFAULT 'bank_transfer'
                    CHECK (method IN ('bank_transfer', 'card', 'cash', 'cheque', 'stripe', 'other')),
    paid_on         date NOT NULL,
    reference       text CHECK (length(reference) <= 200),
    organisation_id uuid,
    talent_id       uuid,
    booking_id      uuid,
    -- The payment a refund returns.
    refund_of       uuid,
    -- How much of a payment is allocated to invoices (or refunded).
    allocated_minor bigint NOT NULL DEFAULT 0 CHECK (allocated_minor >= 0),
    notes           text CHECK (length(notes) <= 5000),
    voided_at       timestamptz,
    void_reason     text CHECK (length(void_reason) <= 500),
    version         bigint NOT NULL DEFAULT 1,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    created_by      uuid REFERENCES users (id),
    updated_by      uuid REFERENCES users (id),
    UNIQUE (workspace_id, id),
    CHECK (allocated_minor <= amount_minor),
    CHECK ((kind = 'refund') = (refund_of IS NOT NULL)),
    CHECK (kind <> 'payout' OR talent_id IS NOT NULL),
    FOREIGN KEY (workspace_id, organisation_id) REFERENCES organisations (workspace_id, id),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id),
    FOREIGN KEY (workspace_id, booking_id) REFERENCES bookings (workspace_id, id),
    FOREIGN KEY (workspace_id, refund_of) REFERENCES payments (workspace_id, id)
);
CREATE INDEX payments_workspace ON payments (workspace_id, paid_on DESC);
CREATE INDEX payments_booking ON payments (workspace_id, booking_id);
CREATE INDEX payments_talent ON payments (workspace_id, talent_id);
CREATE TRIGGER payments_updated_at BEFORE UPDATE ON payments FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER payments_version BEFORE UPDATE ON payments FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Which invoices a payment pays, and how much of each. Taken back by
-- marking `removed_at`, never by deleting.
CREATE TABLE payment_allocations (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    payment_id   uuid NOT NULL,
    invoice_id   uuid NOT NULL,
    amount_minor bigint NOT NULL CHECK (amount_minor > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    removed_at   timestamptz,
    removed_by   uuid REFERENCES users (id),
    FOREIGN KEY (workspace_id, payment_id) REFERENCES payments (workspace_id, id),
    FOREIGN KEY (workspace_id, invoice_id) REFERENCES invoices (workspace_id, id)
);
CREATE INDEX payment_allocations_payment ON payment_allocations (payment_id) WHERE removed_at IS NULL;
CREATE INDEX payment_allocations_invoice ON payment_allocations (invoice_id) WHERE removed_at IS NULL;

-- Settlements ----------------------------------------------------------------------------

-- What one talent is owed for one booking: the gross fee, less the
-- agency's commission and expenses.
CREATE TABLE settlements (
    id                    uuid PRIMARY KEY,
    workspace_id          uuid NOT NULL REFERENCES workspaces (id),
    booking_id            uuid NOT NULL,
    talent_id             uuid NOT NULL,
    status                text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'final')),
    currency              char(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    gross_minor           bigint NOT NULL CHECK (gross_minor >= 0),
    commission_bps        integer NOT NULL CHECK (commission_bps BETWEEN 0 AND 10000),
    commission_minor      bigint NOT NULL CHECK (commission_minor >= 0),
    -- [{description, amount_minor}]: costs met by the agency, taken from the talent's share.
    expenses              jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(expenses) = 'array'),
    expenses_minor        bigint NOT NULL DEFAULT 0 CHECK (expenses_minor >= 0),
    net_minor             bigint NOT NULL,
    notes                 text CHECK (length(notes) <= 5000),
    finalised_at          timestamptz,
    finalised_by          uuid REFERENCES users (id),
    -- The statement, and (when the talent invoices the buyer) the commission invoice.
    document_id           uuid,
    commission_invoice_id uuid,
    version               bigint NOT NULL DEFAULT 1,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    created_by            uuid REFERENCES users (id),
    updated_by            uuid REFERENCES users (id),
    deleted_at            timestamptz,
    UNIQUE (workspace_id, id),
    CHECK ((status = 'final') = (finalised_at IS NOT NULL)),
    CHECK (net_minor = gross_minor - commission_minor - expenses_minor),
    FOREIGN KEY (workspace_id, booking_id) REFERENCES bookings (workspace_id, id),
    FOREIGN KEY (workspace_id, talent_id) REFERENCES talent (workspace_id, id),
    FOREIGN KEY (workspace_id, document_id) REFERENCES documents (workspace_id, id),
    FOREIGN KEY (workspace_id, commission_invoice_id) REFERENCES invoices (workspace_id, id)
);
CREATE UNIQUE INDEX settlements_one_per_talent ON settlements (workspace_id, booking_id, talent_id) WHERE deleted_at IS NULL;
CREATE TRIGGER settlements_updated_at BEFORE UPDATE ON settlements FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER settlements_version BEFORE UPDATE ON settlements FOR EACH ROW EXECUTE FUNCTION bump_version();

ALTER TABLE payments ADD COLUMN settlement_id uuid;
ALTER TABLE payments ADD FOREIGN KEY (workspace_id, settlement_id) REFERENCES settlements (workspace_id, id);

-- Reading past PDFs ----------------------------------------------------------------------

-- One uploaded PDF on its way to becoming a document (and perhaps a past
-- invoice): read, matched to a record, then confirmed by a person.
CREATE TABLE pdf_imports (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    file_id      uuid NOT NULL,
    status       text NOT NULL DEFAULT 'reading'
                 CHECK (status IN ('reading', 'ready', 'confirmed', 'discarded', 'failed')),
    -- What the reading found: the kind guessed, field suggestions with their evidence.
    found        jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(found) = 'object'),
    -- Records it may belong to, best first: [{entity, id, name, reason}].
    matches      jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(matches) = 'array'),
    error        text CHECK (length(error) <= 1000),
    document_id  uuid,
    invoice_id   uuid,
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, file_id) REFERENCES files (workspace_id, id),
    FOREIGN KEY (workspace_id, document_id) REFERENCES documents (workspace_id, id),
    FOREIGN KEY (workspace_id, invoice_id) REFERENCES invoices (workspace_id, id)
);
CREATE INDEX pdf_imports_workspace ON pdf_imports (workspace_id, status, created_at DESC);
CREATE TRIGGER pdf_imports_updated_at BEFORE UPDATE ON pdf_imports FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER pdf_imports_version BEFORE UPDATE ON pdf_imports FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Field patterns for issuers whose PDFs arrive often (see core_pdf::templates).
CREATE TABLE pdf_templates (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    name         text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    definition   jsonb NOT NULL CHECK (jsonb_typeof(definition) = 'object'),
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    archived_at  timestamptz
);
CREATE INDEX pdf_templates_workspace ON pdf_templates (workspace_id) WHERE archived_at IS NULL;
CREATE TRIGGER pdf_templates_updated_at BEFORE UPDATE ON pdf_templates FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER pdf_templates_version BEFORE UPDATE ON pdf_templates FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Row-level security ----------------------------------------------------------------------

ALTER TABLE files ENABLE ROW LEVEL SECURITY;
ALTER TABLE file_blobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
ALTER TABLE document_versions ENABLE ROW LEVEL SECURITY;
ALTER TABLE signature_requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE tax_codes ENABLE ROW LEVEL SECURITY;
ALTER TABLE finance_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE invoice_sequences ENABLE ROW LEVEL SECURITY;
ALTER TABLE invoices ENABLE ROW LEVEL SECURITY;
ALTER TABLE payments ENABLE ROW LEVEL SECURITY;
ALTER TABLE payment_allocations ENABLE ROW LEVEL SECURITY;
ALTER TABLE settlements ENABLE ROW LEVEL SECURITY;
ALTER TABLE pdf_imports ENABLE ROW LEVEL SECURITY;
ALTER TABLE pdf_templates ENABLE ROW LEVEL SECURITY;
-- Shared reference data: readable by the app, written by the worker.
ALTER TABLE fx_rates ENABLE ROW LEVEL SECURITY;

CREATE POLICY files_tenant ON files FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY file_blobs_tenant ON file_blobs FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY documents_tenant ON documents FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY document_versions_tenant ON document_versions FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY signature_requests_tenant ON signature_requests FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY tax_codes_tenant ON tax_codes FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY finance_settings_tenant ON finance_settings FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY invoice_sequences_tenant ON invoice_sequences FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY invoices_tenant ON invoices FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY payments_tenant ON payments FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY payment_allocations_tenant ON payment_allocations FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY settlements_tenant ON settlements FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY pdf_imports_tenant ON pdf_imports FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY pdf_templates_tenant ON pdf_templates FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY fx_rates_read ON fx_rates FOR SELECT TO onhold_app USING (true);

-- The worker fetches exchange rates, finds invoices passing their due date
-- (ids and dates only), and deletes files nothing uses (their bytes in
-- `file_blobs` go with them, by the cascade).
CREATE POLICY fx_rates_worker ON fx_rates FOR ALL TO onhold_worker USING (true) WITH CHECK (true);
CREATE POLICY invoices_worker ON invoices FOR SELECT TO onhold_worker
    USING (status IN ('issued', 'part_paid') AND deleted_at IS NULL);
CREATE POLICY files_worker ON files FOR SELECT TO onhold_worker
    USING (attached_at IS NULL AND deleted_at IS NULL);
CREATE POLICY files_worker_delete ON files FOR DELETE TO onhold_worker
    USING (attached_at IS NULL AND deleted_at IS NULL);

-- Grants ------------------------------------------------------------------------------------

GRANT SELECT, INSERT, UPDATE ON files, documents, document_versions, signature_requests, tax_codes,
    finance_settings, invoice_sequences, invoices, payments, payment_allocations, settlements,
    pdf_imports, pdf_templates TO onhold_app;
GRANT SELECT, INSERT ON file_blobs TO onhold_app;
GRANT SELECT ON fx_rates TO onhold_app;
GRANT EXECUTE ON FUNCTION signature_public_find(bytea), invoice_public_find(bytea) TO onhold_app;

GRANT SELECT, INSERT, UPDATE ON fx_rates TO onhold_worker;
GRANT SELECT (id, workspace_id, kind, status, due_date, deleted_at) ON invoices TO onhold_worker;
GRANT SELECT (id, workspace_id, storage, storage_key, attached_at, created_at, deleted_at), DELETE
    ON files TO onhold_worker;
