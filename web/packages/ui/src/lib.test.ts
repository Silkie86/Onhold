import { applyBrandColour, brandColours, darkBrandColour, hexColour, logoSource, readBranding } from "./lib.ts";

test("brand colours are hex only, with readable text on them", () => {
  expect(hexColour(" #AA00ff ")).toBe("#aa00ff");
  expect(hexColour("#abc")).toBe("#abc");
  expect(hexColour("red")).toBeNull();
  expect(hexColour("#abc; background: url(x)")).toBeNull();
  expect(brandColours("#1d3fbf")["primary-foreground"]).toBe("#ffffff");
  expect(brandColours("#ffd400")["primary-foreground"]).toBe("#111111");
  expect(brandColours("nope")).toEqual({});
});

test("logos are image data URIs only", () => {
  expect(logoSource("data:image/png;base64,iVBORw0KGgo=")).toBe("data:image/png;base64,iVBORw0KGgo=");
  expect(logoSource("https://example.com/logo.png")).toBeNull();
  expect(logoSource("data:text/html;base64,PHNjcmlwdD4=")).toBeNull();
  expect(logoSource('data:image/png;base64,"><script>')).toBeNull();
});

test("stored branding reads as a logo and a colour", () => {
  expect(readBranding({ primary_color: "#C0392B", logo: "data:image/png;base64,iVBORw0KGgo=" })).toEqual({
    logo: "data:image/png;base64,iVBORw0KGgo=",
    colour: "#c0392b",
  });
  expect(readBranding({ primary_color: "red" })).toEqual({ logo: null, colour: null });
  expect(readBranding(null)).toEqual({ logo: null, colour: null });
});

test("dark mode gets a lighter shade of a dark brand colour", () => {
  const dark = darkBrandColour("#1e3a8a");
  expect(dark).not.toBe("#1e3a8a");
  expect(dark).toMatch(/^#[0-9a-f]{6}$/);
  // Already light enough: kept.
  expect(darkBrandColour("#ffd400")).toBe("#ffd400");
});

test("the brand colour is applied as a style with a dark-mode shade, and removed again", () => {
  applyBrandColour("#1e3a8a");
  const css = document.getElementById("onhold-brand")?.textContent ?? "";
  expect(css).toContain("--primary: #1e3a8a;");
  expect(css).toContain(":root.dark");
  expect(css).toContain(`--primary: ${darkBrandColour("#1e3a8a")};`);
  applyBrandColour(null);
  expect(document.getElementById("onhold-brand")).toBeNull();
});
