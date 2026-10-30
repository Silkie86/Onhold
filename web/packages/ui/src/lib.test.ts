import {
  applyBrandColour,
  applyBrandTheme,
  brandColours,
  contrastRatio,
  darkBrandColour,
  hexColour,
  logoSource,
  readBranding,
  readTheme,
} from "./lib.ts";

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
    theme: null,
  });
  expect(readBranding({ primary_color: "red" })).toEqual({ logo: null, colour: null, theme: null });
  expect(readBranding(null)).toEqual({ logo: null, colour: null, theme: null });
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

test("a full theme goes on top of the brand colour, per mode, with its font and corners", () => {
  const theme = readTheme({
    light: { primary: "#112233", background: "#fffaf5", sidebar: "#000000", muted: "nope" },
    dark: { background: "#101010" },
    font: "serif",
    radius: 12,
  });
  expect(theme).toEqual({ light: { primary: "#112233", background: "#fffaf5" }, dark: { background: "#101010" }, font: "serif", radius: 12 });
  applyBrandTheme({ colour: "#c0392b", theme });
  const css = document.getElementById("onhold-brand")?.textContent ?? "";
  expect(css).toContain("--primary: #112233;");
  expect(css).toContain("--background: #fffaf5;");
  expect(css).toContain("--background: #101010;");
  expect(css).toContain("--radius: 12px;");
  expect(css).toContain("Charter");
  expect(readTheme({ font: "Comic Sans", radius: 99 })).toEqual({ light: {}, dark: {} });
  expect(contrastRatio("#000000", "#ffffff")).toBeCloseTo(21, 0);
  // A colour set for light alone stays out of dark mode, whichever way it is chosen.
  applyBrandTheme({ colour: null, theme: { light: { card: "#fff8ee" }, dark: {} } });
  const light = document.getElementById("onhold-brand")?.textContent ?? "";
  expect(light).toContain(":root.light { --card: #fff8ee; }");
  expect(light).toContain(":root:not(.dark) { --card: #fff8ee; }");
  expect(light).not.toMatch(/^:root \{[^}]*--card/);
  expect(light).toContain(":root.dark {  }");
  // A danger colour set alone gets readable text, whatever the default text is.
  applyBrandTheme({ colour: null, theme: { light: {}, dark: { destructive: "#b91c1c" } } });
  expect(document.getElementById("onhold-brand")?.textContent).toContain("--destructive-foreground: #ffffff;");
  applyBrandTheme({ colour: null, theme: { light: {}, dark: { destructive: "#fca5a5", "destructive-foreground": "#450a0a" } } });
  expect(document.getElementById("onhold-brand")?.textContent).toContain("--destructive-foreground: #450a0a;");
  applyBrandTheme({ colour: null });
});
