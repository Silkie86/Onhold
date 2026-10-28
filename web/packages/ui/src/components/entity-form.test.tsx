import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { contactSchema } from "../fixtures.ts";
import { EntityForm, formBody, parseList } from "./entity-form.tsx";

test("a new record sends what was filled in, custom fields under custom", async () => {
  const sent: Record<string, unknown>[] = [];
  render(<EntityForm schema={contactSchema} onSubmit={async (b) => void sent.push(b)} />);
  await userEvent.type(screen.getByLabelText(/^Name/), "Jo Booker");
  await userEvent.type(screen.getByLabelText("Emails"), "jo@example.com, jo@home.example");
  await userEvent.type(screen.getByLabelText("Budget"), "1,200.50");
  await userEvent.selectOptions(screen.getByLabelText("Genre"), "jazz");
  await userEvent.click(screen.getByRole("button", { name: "Create" }));
  expect(sent).toEqual([
    {
      name: "Jo Booker",
      emails: ["jo@example.com", "jo@home.example"],
      email_opt_in: false,
      custom: { budget: { minor: 120050, currency: "GBP" }, genre: "jazz" },
    },
  ]);
});

test("custom fields are grouped and the server's problems shown by field", () => {
  render(
    <EntityForm
      schema={contactSchema}
      onSubmit={async () => {}}
      issues={[
        { field: "emails", message: "not-an-email is not an email address" },
        { field: "custom.budget", message: "must be at least 0" },
        { field: "somewhere_else", message: "is wrong" },
      ]}
    />,
  );
  expect(screen.getByRole("group", { name: "Deal" })).toBeTruthy();
  expect(screen.getByText("not-an-email is not an email address")).toBeTruthy();
  expect(screen.getByText("must be at least 0")).toBeTruthy();
  expect(screen.getByRole("alert").textContent).toBe("somewhere_else: is wrong");
  expect(screen.getByLabelText("Emails").getAttribute("aria-invalid")).toBe("true");
});

test("an edit sends only what changed", () => {
  const record = { id: "1", name: "Jo", emails: ["jo@example.com"], tags: ["vip"], email_opt_in: false, organisation_id: null, custom: { genre: "jazz" } };
  const values = { name: "Jo Booker", emails: ["jo@example.com"], tags: ["vip"], email_opt_in: false, organisation_id: null, "custom.budget": null, "custom.genre": "rock" };
  expect(formBody(contactSchema, values, record)).toEqual({ name: "Jo Booker", custom: { genre: "rock" } });
});

test("someone who cannot write sees the values but no buttons", () => {
  render(
    <EntityForm
      schema={{ ...contactSchema, can_write: false }}
      record={{ id: "1", name: "Jo", custom: {} }}
      onSubmit={async () => {}}
    />,
  );
  expect((screen.getByLabelText(/^Name/) as HTMLInputElement).disabled).toBe(true);
  expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
});

test("web addresses are split on spaces, so commas in them survive", () => {
  expect(parseList("a, b\nc")).toEqual(["a", "b", "c"]);
  expect(parseList("example.com/a,b  instagram.com/x", true)).toEqual(["example.com/a,b", "instagram.com/x"]);
});
