import { safeRedirect } from "./auth.tsx";
import { decodeFile, stripCardAttachments } from "./imports.tsx";
import { validateListSearch } from "./crm.tsx";
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
  expect(decodeFile(new Uint8Array([0xff, 0xfe, 0x41, 0x00, 0x09, 0x00, 0x42, 0x00]))).toBe("A\tB");
});

test("contact cards lose their photos before upload", () => {
  const card = "BEGIN:VCARD\r\nFN:Ada\r\nPHOTO;ENCODING=b:AAAA\r\n BBBB\r\nitem1.EMAIL:ada@example.com\r\nEND:VCARD";
  expect(stripCardAttachments(card)).toBe("BEGIN:VCARD\r\nFN:Ada\r\nitem1.EMAIL:ada@example.com\r\nEND:VCARD");
  expect(stripCardAttachments("Name,Photo\nAda,x")).toBe("Name,Photo\nAda,x");
});
