import { safeRedirect } from "./auth.tsx";
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
