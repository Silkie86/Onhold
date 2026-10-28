-- Travel and money on a booking. A booking (a gig, a shoot, a session) can
-- carry the travel that gets people there and back: flights with their
-- numbers, airports and times, trains and ground transport, and hotels.
-- It can also carry extra income beside the fee (a hotel or catering
-- buyout) and the costs it brings (per diems, crew, production), so the
-- booking page can show whether the fee still makes sense once everything
-- is paid for.
--
-- Only text and numbers are kept: no airport tables, no tickets or
-- boarding passes copied in (attach those as documents if needed).

CREATE TABLE travel_items (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces (id),
    booking_id    uuid NOT NULL,
    kind          text NOT NULL CHECK (kind IN ('flight', 'train', 'bus', 'car', 'ferry', 'taxi', 'hotel', 'other')),
    -- planned: still to book. booked: paid for or confirmed. cancelled: kept for the record.
    status        text NOT NULL DEFAULT 'planned' CHECK (status IN ('planned', 'booked', 'cancelled')),
    -- The airline, rail company, driver or hotel.
    carrier       text CHECK (length(carrier) <= 100),
    -- A flight or train number: "BA 1442".
    number        text CHECK (length(number) <= 20),
    -- The booking reference or confirmation code.
    reference     text CHECK (length(reference) <= 50),
    -- Where from and to: an airport or station name, an address. Codes are
    -- IATA airport codes when known ("LHR"). A hotel uses `to_*` for itself.
    from_place    text CHECK (length(from_place) <= 200),
    from_code     text CHECK (from_code ~ '^[A-Z]{3}$'),
    to_place      text CHECK (length(to_place) <= 200),
    to_code       text CHECK (to_code ~ '^[A-Z]{3}$'),
    -- Departure and arrival (check-in and check-out for a hotel), each
    -- shown in its own place's zone.
    starts_at     timestamptz NOT NULL,
    start_tz      text NOT NULL CHECK (length(start_tz) BETWEEN 1 AND 64),
    ends_at       timestamptz,
    end_tz        text CHECK (length(end_tz) BETWEEN 1 AND 64),
    -- Who travels: names as written, so crew without a record can be listed.
    travellers    text[] NOT NULL DEFAULT '{}' CHECK (cardinality(travellers) <= 40),
    -- Seats, baggage, room types, pick-up instructions.
    notes         text CHECK (length(notes) <= 2000),
    cost_minor    bigint CHECK (cost_minor >= 0),
    cost_currency char(3) CHECK (cost_currency ~ '^[A-Z]{3}$'),
    -- us: the act, the agency or the workspace pays (a cost against the fee).
    -- buyer: the promoter or client provides it (no cost here).
    paid_by       text NOT NULL DEFAULT 'us' CHECK (paid_by IN ('us', 'buyer')),
    version       bigint NOT NULL DEFAULT 1,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    created_by    uuid REFERENCES users (id),
    updated_by    uuid REFERENCES users (id),
    deleted_at    timestamptz,
    UNIQUE (workspace_id, id),
    CHECK (ends_at IS NULL OR ends_at >= starts_at),
    CHECK ((ends_at IS NULL) = (end_tz IS NULL)),
    CHECK ((cost_minor IS NULL) = (cost_currency IS NULL)),
    FOREIGN KEY (workspace_id, booking_id) REFERENCES bookings (workspace_id, id)
);
CREATE INDEX travel_items_booking ON travel_items (workspace_id, booking_id, starts_at) WHERE deleted_at IS NULL;
CREATE TRIGGER travel_items_updated_at BEFORE UPDATE ON travel_items FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER travel_items_version BEFORE UPDATE ON travel_items FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Income beside the fee (buyouts, merchandise) and costs a booking brings.
CREATE TABLE money_lines (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id),
    booking_id   uuid NOT NULL,
    direction    text NOT NULL CHECK (direction IN ('income', 'cost')),
    category     text NOT NULL CHECK (category ~ '^[a-z_]{1,30}$'),
    description  text NOT NULL CHECK (length(btrim(description)) BETWEEN 1 AND 200),
    amount_minor bigint NOT NULL CHECK (amount_minor >= 0),
    currency     char(3) NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    spent_on     date,
    notes        text CHECK (length(notes) <= 2000),
    version      bigint NOT NULL DEFAULT 1,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid REFERENCES users (id),
    updated_by   uuid REFERENCES users (id),
    deleted_at   timestamptz,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, booking_id) REFERENCES bookings (workspace_id, id)
);
CREATE INDEX money_lines_booking ON money_lines (workspace_id, booking_id) WHERE deleted_at IS NULL;
CREATE TRIGGER money_lines_updated_at BEFORE UPDATE ON money_lines FOR EACH ROW EXECUTE FUNCTION set_updated_at();
CREATE TRIGGER money_lines_version BEFORE UPDATE ON money_lines FOR EACH ROW EXECUTE FUNCTION bump_version();

ALTER TABLE travel_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE money_lines ENABLE ROW LEVEL SECURITY;
CREATE POLICY travel_items_tenant ON travel_items FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
CREATE POLICY money_lines_tenant ON money_lines FOR ALL TO onhold_app
    USING (workspace_id = app_workspace_id()) WITH CHECK (workspace_id = app_workspace_id());
GRANT SELECT, INSERT, UPDATE ON travel_items, money_lines TO onhold_app;

-- A day sheet is a document of its own kind.
ALTER TABLE documents DROP CONSTRAINT documents_kind_check;
ALTER TABLE documents ADD CONSTRAINT documents_kind_check CHECK (kind IN (
    'contract', 'confirmation', 'advance_sheet', 'day_sheet', 'invoice', 'credit_note', 'quote', 'statement',
    'rider', 'tech_spec', 'stage_plot', 'epk', 'insurance', 'passport', 'visa', 'receipt',
    'purchase_order', 'misc'));
