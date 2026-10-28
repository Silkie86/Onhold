import { execFileSync } from "node:child_process";

/** Creates a fresh workspace for each run and hands its owner's invitation to the tests. */
export default function setup() {
  const bin = process.env.ONHOLD_BIN ?? "../../../target/debug/onhold";
  const runs = ["desktop", "phone", "app", "installed", "installed-bookings", "bookings-desktop", "bookings-phone", "inbox-desktop", "inbox-phone", "installed-inbox", "branding-desktop", "branding-phone", "theme-desktop", "theme-phone", "setup-desktop", "setup-phone"];
  for (const project of runs) {
    const slug = `e2e-${project}-${Date.now().toString(36)}`;
    const out = execFileSync(
      bin,
      [
        "admin", "workspace", "create",
        "--slug", slug,
        "--name", `E2E ${project}`,
        "--country", "GB",
        "--currency", "GBP",
        "--timezone", "Europe/London",
        "--owner-email", `owner@${slug}.test`,
      ],
      { encoding: "utf8", env: { ...process.env, PUBLIC_BASE_URL: process.env.E2E_BASE_URL ?? "http://localhost:5173" } },
    );
    const link = out.match(/owner invitation: (\S+)/)?.[1];
    if (!link) throw new Error(`no invitation link in:\n${out}`);
    process.env[`E2E_INVITE_${project.toUpperCase().replace("-", "_")}`] = new URL(link).pathname + new URL(link).search;
  }
}
