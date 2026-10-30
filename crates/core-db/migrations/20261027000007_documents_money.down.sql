-- Reverts M5: documents and money.

DROP TABLE pdf_templates;
DROP TABLE pdf_imports;
ALTER TABLE payments DROP COLUMN settlement_id;
DROP TABLE settlements;
DROP TABLE payment_allocations;
DROP TABLE payments;
DROP FUNCTION invoice_public_find(bytea);
DROP TRIGGER invoices_freeze_issued ON invoices;
DROP FUNCTION invoices_freeze_issued();
DROP TABLE invoices;
DROP TYPE invoice_status;
DROP TABLE fx_rates;
DROP TABLE invoice_sequences;
DROP TABLE finance_settings;
DROP TABLE tax_codes;
DROP FUNCTION signature_public_find(bytea);
DROP TRIGGER signature_requests_freeze_signed ON signature_requests;
DROP FUNCTION signature_requests_freeze_signed();
DROP TABLE signature_requests;
DROP TABLE document_versions;
DROP TABLE documents;
DROP TABLE file_blobs;
DROP TABLE files;

ALTER TABLE bookings DROP COLUMN fx_rate;
ALTER TABLE talent DROP COLUMN billing;

DELETE FROM imports WHERE entity = 'invoice';
ALTER TABLE imports DROP CONSTRAINT imports_entity_check;
ALTER TABLE imports ADD CONSTRAINT imports_entity_check
    CHECK (entity IN ('talent', 'organisation', 'contact'));

UPDATE tasks SET related_entity = NULL, related_id = NULL WHERE related_entity IN ('invoice', 'document');
ALTER TABLE tasks DROP CONSTRAINT tasks_related_entity_check;
ALTER TABLE tasks ADD CONSTRAINT tasks_related_entity_check
    CHECK (related_entity IN ('talent', 'organisation', 'contact', 'enquiry', 'booking', 'quote'));

DELETE FROM templates WHERE kind = 'document';
ALTER TABLE templates DROP CONSTRAINT templates_kind_check;
ALTER TABLE templates ADD CONSTRAINT templates_kind_check
    CHECK (kind IN ('email', 'sms', 'whatsapp', 'notification', 'partial'));

DELETE FROM role_permissions WHERE permission IN (
    'document:read', 'document:write', 'invoice:read', 'invoice:write', 'payment:write',
    'report:read', 'finance:config');
