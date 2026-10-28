import { brandColours, hexColour, logoSource, readBranding } from "./lib.ts";

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

test("stored branding reads as a logo and tokens", () => {
  expect(readBranding({ primary_color: "#C0392B", logo: "data:image/png;base64,iVBORw0KGgo=" })).toEqual({
    logo: "data:image/png;base64,iVBORw0KGgo=",
    colours: { primary: "#c0392b", ring: "#c0392b", "primary-foreground": "#ffffff" },
  });
  expect(readBranding(null)).toEqual({ logo: null, colours: {} });
});
