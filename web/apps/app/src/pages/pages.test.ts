import { safeRedirect } from "./auth.tsx";
import { canUndo, decodeFile, isWorkbook, needsResaving, rowLabel, sheetsSummary, stripCardAttachments } from "./imports.tsx";
import { dialable, phonesOf } from "../phones.ts";
import { validateListSearch } from "./crm.tsx";
import { awaitingOutcome, dayOf, happenedAt, keyFor, mailtoLink, newKey, smsLink, uniqueItems, unfilled, whatsappLink } from "./pitching.tsx";
import { keyFrom } from "./settings.tsx";

test("after signing in, only paths on this site are followed", () => {
  expect(safeRedirect("/crm/contacts?q=jo")).toBe("/crm/contacts?q=jo");
  expect(safeRedirect("https://evil.example/")).toBe("/");
  expect(safeRedirect("//evil.example/")).toBe("/");
  expect(safeRedirect(undefined)).toBe("/");
});

test("field keys are made from labels", () => {
  expect(keyFrom("Dietary needs")).toBe("dietary_needs");
  expect(keyFrom("Café / bar?")).toBe("cafe_bar");
  expect(keyFrom("2nd phone")).toBe("f_2nd_phone");
  expect(keyFrom("!!!")).toBe("");
});

test("pitching keys are made from names and never repeat", () => {
  expect(keyFor("North America", [])).toBe("north_america");
  expect(keyFor("UK", ["uk"])).toBe("uk_2");
  expect(keyFor("UK", ["uk", "uk_2"])).toBe("uk_3");
  expect(keyFor("", [])).toBe("item");
  // New ones never take the key of one removed before.
  expect(newKey("status", [], 1_700_000_000_000)).toBe("status_loyw3v28");
  expect(newKey("status", [], 1_700_000_000_000)).not.toBe(newKey("status", [], 1_700_000_000_001));
});

test("numbers are dialled without extensions or second numbers", () => {
  expect(dialable("+44 20 7946 0000")).toBe("+442079460000");
  expect(dialable("020 7946 0000 ext 12")).toBe("02079460000");
  expect(dialable("020 7946 0000 / 07700 900123")).toBe("02079460000");
});

test("list addresses keep only what they understand", () => {
  expect(validateListSearch({ q: "jo", offset: "50", limit: 25, view: "", junk: 1 })).toEqual({
    q: "jo",
    sort: undefined,
    tag: undefined,
    offset: 50,
    limit: 25,
    view: undefined,
  });
});

test("imported files are read whatever they were saved as", () => {
  const utf8 = new TextEncoder().encode("Name\nJürgen");
  expect(decodeFile(utf8)).toBe("Name\nJürgen");
  // Windows-1252, as older Outlook exports are: ü is 0xFC.
  expect(decodeFile(new Uint8Array([0x4e, 0x0a, 0x4a, 0xfc, 0x72]))).toBe("N\nJür");
  // UTF-16 with its byte-order mark, as Excel's "Unicode text" is.
  // UTF-8 with one stray byte stays UTF-8.
  const mostly = new Uint8Array([...new TextEncoder().encode("Jürgen Müller Café Zoë Ñandú "), 0xff]);
  expect(decodeFile(mostly).startsWith("Jürgen")).toBe(true);
  expect(decodeFile(new Uint8Array([0xff, 0xfe, 0x41, 0x00, 0x09, 0x00, 0x42, 0x00]))).toBe("A\tB");
});

test("contact cards lose their photos before upload", () => {
  const card = "BEGIN:VCARD\r\nFN:Ada\r\nPHOTO;ENCODING=b:AAAA\r\n BBBB\r\nitem1.EMAIL:ada@example.com\r\nEND:VCARD";
  expect(stripCardAttachments(card)).toBe("BEGIN:VCARD\nFN:Ada\nitem1.EMAIL:ada@example.com\nEND:VCARD");
  // A quoted-printable note carrying on into a line that looks like a photo keeps it.
  const qp = "BEGIN:VCARD\nNOTE;ENCODING=QUOTED-PRINTABLE:see=\nphoto: soon\nEND:VCARD";
  expect(stripCardAttachments(qp)).toBe(qp);
  expect(stripCardAttachments("Name,Photo\nAda,x")).toBe("Name,Photo\nAda,x");
});

test("Excel files are told apart from text", () => {
  expect(isWorkbook(new Uint8Array([0x50, 0x4b, 0x03, 0x04, 0x14]))).toBe(true);
  expect(isWorkbook(new Uint8Array([0xd0, 0xcf, 0x11, 0xe0, 0xa1]))).toBe(false);
  // Old .xls (or a web page saved as .xls) and OpenDocument need saving as .xlsx first.
  expect(needsResaving("export.xls", new TextEncoder().encode("<html>"))).toBe(true);
  expect(needsResaving("book.ods", new Uint8Array([0x50, 0x4b, 0x03, 0x04]))).toBe(true);
  expect(needsResaving("book.xlsx", new Uint8Array([0x50, 0x4b, 0x03, 0x04]))).toBe(false);
  expect(needsResaving("people.csv", new TextEncoder().encode("Name"))).toBe(false);
  expect(isWorkbook(new TextEncoder().encode("PK,Name\n1,Ada"))).toBe(false);
});

test("a workbook's sheets are summed up in a sentence", () => {
  expect(sheetsSummary([])).toBeNull();
  expect(sheetsSummary([{ name: "Sheet1", rows: 3, included: true }])).toBeNull();
  expect(
    sheetsSummary([
      { name: "UK & Ireland", rows: 4, included: true },
      { name: "Europe", rows: 1, included: true },
      { name: "Notes", rows: 1, included: false },
    ]),
  ).toBe(
    "Read 2 sheets: UK & Ireland (4 rows), Europe (1 row). Each row's sheet is in the Sheet column, which can go into Region, and its row there in the Sheet row column. Left out, as their columns are different: Notes.",
  );
});

test("only finished imports with records left to undo can be undone", () => {
  expect(canUndo({ status: "done", to_undo: 3 })).toBe(true);
  expect(canUndo({ status: "failed", to_undo: 1 })).toBe(true);
  // From before undoing existed, or nothing created.
  expect(canUndo({ status: "done", to_undo: 0 })).toBe(false);
  expect(canUndo({ status: "running", to_undo: 5 })).toBe(false);
  expect(canUndo({ status: "undoing", to_undo: 5 })).toBe(false);
  expect(canUndo({ status: "undone", to_undo: 2 })).toBe(false);
});

test("rows are named where they can be found", () => {
  expect(rowLabel({ row: 7 })).toBe("Row 7");
  expect(rowLabel({ row: 7, at: "Europe, row 2" })).toBe("Europe, row 2");
});

test("a logged entry happens now today, or at midday on another day", () => {
  const now = new Date(2026, 9, 28, 15, 30);
  expect(dayOf(now.toISOString())).toBe("2026-10-28");
  expect(happenedAt("2026-10-28", now)).toBe(now.toISOString());
  expect(happenedAt("2026-10-21", now)).toBe(new Date(2026, 9, 21, 12, 0).toISOString());
});

test("an act on both of an organisation's lists is offered once", () => {
  const owls = { kind: "talent" as const, id: "1", name: "Owls" };
  const tour = { kind: "project" as const, id: "1", name: "Tour" };
  expect(uniqueItems([owls, tour, { ...owls }])).toEqual([owls, tour]);
});

test("an answered pitch asks what they said until an answer is logged", () => {
  const base = { organisation_id: "o", created_at: "2026-10-01T09:00:00Z", source: "sent", type_key: "pitched" };
  const pitch = { ...base, id: "a", kind: "action" as const, happened_at: "2026-10-01T09:00:00Z", answered_at: "2026-10-03T10:00:00Z" };
  const before = { ...base, id: "b", kind: "outcome" as const, type_key: "no", happened_at: "2026-10-02T10:00:00Z" };
  const after = { ...before, id: "c", happened_at: "2026-10-03T10:00:00Z" };
  expect(awaitingOutcome(pitch, [pitch, before])).toBe(true);
  expect(awaitingOutcome(pitch, [after, pitch])).toBe(false);
  expect(awaitingOutcome({ ...pitch, answered_at: null }, [pitch])).toBe(false);
});

test("numbers changed offline are dialled as written, not as the server last wrote them", () => {
  expect(phonesOf({ phones: ["07700 900123"], phones_international: ["+44 7700 900123"] })).toEqual(["+44 7700 900123"]);
  expect(phonesOf({ phones: ["07700 900123", "0113 496 0000"], phones_international: ["+44 7700 900123"] })).toEqual(["07700 900123", "0113 496 0000"]);
});

test("a pitch opens in an email app, a text or WhatsApp ready to send", () => {
  expect(mailtoLink("jo@club.test", "Owls & you", "Hi Jo,\nDates?")).toBe(
    "mailto:jo@club.test?subject=Owls%20%26%20you&body=Hi%20Jo%2C%0ADates%3F",
  );
  expect(mailtoLink(null, "S", "B")).toBe("mailto:?subject=S&body=B");
  expect(whatsappLink("+44 7700 900123", "Hi Jo")).toBe("https://wa.me/447700900123?text=Hi%20Jo");
  // Without the country, WhatsApp asks who to send it to.
  expect(whatsappLink("07700 900123", "Hi")).toBe("https://wa.me/?text=Hi");
  expect(smsLink("+44 7700 900123", "Hi Jo")).toBe("sms:+447700900123?&body=Hi%20Jo");
});

test("placeholders left in a pitch are found once each", () => {
  expect(unfilled("Hi {firstName}, see {link} {link} {notOne} {}", ["firstName", "link", "city"])).toEqual(["firstName", "link"]);
});
