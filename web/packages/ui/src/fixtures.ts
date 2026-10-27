import type { EntitySchema } from "./format.ts";

/** A contact schema like the API's, with two custom fields. */
export const contactSchema: EntitySchema = {
  entity: "contact",
  label: "Contact",
  label_plural: "Contacts",
  can_write: true,
  fields: [
    { key: "name", label: "Name", field_type: "text", core: true, required: true, read_only: false, in_list: true, sort: "name", options: [] },
    { key: "emails", label: "Emails", field_type: "email_list", core: true, required: false, read_only: false, in_list: true, sort: "email", options: [] },
    {
      key: "organisation_id", label: "Organisation", field_type: "relation", core: true, required: false, read_only: false,
      in_list: true, sort: "organisation", options: [], relation_entity: "organisation",
    },
    { key: "tags", label: "Tags", field_type: "tags", core: true, required: false, read_only: false, in_list: true, options: [] },
    { key: "email_opt_in", label: "Email opt-in", field_type: "boolean", core: true, required: false, read_only: false, in_list: false, options: [] },
    {
      key: "custom.budget", label: "Budget", field_type: "money", core: false, required: false, read_only: false, in_list: true,
      sort: "custom.budget", options: [], currency: "GBP", group: "Deal",
    },
    {
      key: "custom.genre", label: "Genre", field_type: "select", core: false, required: false, read_only: false, in_list: false,
      sort: "custom.genre", options: [{ value: "jazz", label: "Jazz" }, { value: "rock", label: "Rock" }],
    },
  ],
};
