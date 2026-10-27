import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { contactSchema } from "../fixtures.ts";
import { DataTable, type TableState, defaultTableState } from "./data-table.tsx";

const rows = [
  { id: "a", name: "Jo Booker", emails: ["jo@example.com", "jo@home.example"], organisation_id: "o1", organisation_name: "Blue Room", tags: ["vip"], custom: { budget: { minor: 120050, currency: "GBP" } } },
  { id: "b", name: "Sam Solo", emails: [], organisation_id: null, organisation_name: null, tags: [], custom: {} },
];

function Harness({ onState, onBulk }: { onState: (s: TableState) => void; onBulk?: (ids: string[], tags: string[]) => void }) {
  const [state, setState] = useState(defaultTableState);
  return (
    <DataTable
      schema={contactSchema}
      rows={rows}
      total={2}
      state={state}
      onStateChange={(s) => {
        setState(s);
        onState(s);
      }}
      rowLink={(row, children) => <a href={`/contacts/${row.id}`}>{children}</a>}
      bulkActions={[{ label: "Add tags", needsTags: true, run: async (ids, tags) => onBulk?.(ids, tags) }]}
    />
  );
}

test("shows the schema's list columns, formatted", () => {
  render(<Harness onState={() => {}} />);
  const headers = screen.getAllByRole("columnheader").map((h) => h.textContent);
  expect(headers).toEqual(["", "Name", "Emails", "Organisation", "Tags", "Budget"]);
  expect(screen.getByRole("link", { name: "Jo Booker" })).toHaveProperty("href", expect.stringContaining("/contacts/a"));
  expect(screen.getByText("Blue Room")).toBeTruthy();
  expect(screen.getByText("£1,200.50")).toBeTruthy();
  expect(screen.getByText("+1")).toBeTruthy();
  expect(screen.getByText("1–2 of 2")).toBeTruthy();
});

test("sorting cycles ascending, descending and off", async () => {
  const seen: (string | null)[] = [];
  render(<Harness onState={(s) => seen.push(s.sort)} />);
  const button = screen.getByRole("button", { name: "Budget" });
  await userEvent.click(button);
  await userEvent.click(button);
  await userEvent.click(button);
  expect(seen).toEqual(["custom.budget", "-custom.budget", null]);
});

test("search waits for typing to stop", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  const seen: string[] = [];
  render(<Harness onState={(s) => seen.push(s.q)} />);
  await userEvent.type(screen.getByRole("searchbox"), "jo", { delay: null });
  expect(seen).toEqual([]);
  await act(() => vi.advanceTimersByTimeAsync(350));
  expect(seen).toEqual(["jo"]);
  vi.useRealTimers();
});

test("a custom field can be added as a column", async () => {
  const seen: (string[] | null)[] = [];
  render(<Harness onState={(s) => seen.push(s.columns)} />);
  await userEvent.click(screen.getByRole("button", { name: "Choose columns" }));
  await userEvent.click(await screen.findByRole("menuitemcheckbox", { name: /Genre/ }));
  expect(seen.at(-1)).toContain("custom.genre");
  await userEvent.keyboard("{Escape}");
  expect(screen.getAllByRole("columnheader").map((h) => h.textContent)).toContain("Genre");
});

test("bulk actions run on the selected rows", async () => {
  const runs: [string[], string[]][] = [];
  render(<Harness onState={() => {}} onBulk={(ids, tags) => runs.push([ids, tags])} />);
  const [, second] = screen.getAllByRole("checkbox", { name: "Select row" });
  await userEvent.click(second as HTMLElement);
  const bar = screen.getByRole("toolbar");
  expect(within(bar).getByText("1 selected")).toBeTruthy();
  await userEvent.click(within(bar).getByRole("button", { name: "Add tags" }));
  await userEvent.type(await screen.findByLabelText(/Tags, separated/), "vip, touring");
  await userEvent.click(screen.getByRole("button", { name: "Add tags" }));
  expect(runs).toEqual([[["b"], ["vip", "touring"]]]);
});
