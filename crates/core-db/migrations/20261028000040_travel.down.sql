UPDATE documents SET kind = 'misc' WHERE kind = 'day_sheet';
ALTER TABLE documents DROP CONSTRAINT documents_kind_check;
ALTER TABLE documents ADD CONSTRAINT documents_kind_check CHECK (kind IN (
    'contract', 'confirmation', 'advance_sheet', 'invoice', 'credit_note', 'quote', 'statement',
    'rider', 'tech_spec', 'stage_plot', 'epk', 'insurance', 'passport', 'visa', 'receipt',
    'purchase_order', 'misc'));
DROP TABLE IF EXISTS money_lines;
DROP TABLE IF EXISTS travel_items;
