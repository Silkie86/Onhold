// @vitest-environment node
// Signing out, in a file of its own: once the copy is deleted for good,
// nothing more can be written on this page, which the other tests need.

import "fake-indexeddb/auto";
import type { RecordJson } from "@onhold/ui";
import { afterEach, expect, it, vi } from "vitest";
import { putRecords, setMeta, wipe } from "./store.ts";
import { OfflineSync, stopEverySync } from "./sync.ts";

afterEach(() => {
  vi.unstubAllGlobals();
});

it("stops every sync, and writes nothing to the device after its copy is deleted", async () => {
  vi.stubGlobal("navigator", { onLine: false });
  vi.stubGlobal("window", new EventTarget());
  vi.stubGlobal("document", Object.assign(new EventTarget(), { visibilityState: "visible" }));
  const engine = new OfflineSync("w1");
  await engine.start();
  await putRecords("w1", "contact", [{ id: "c1", name: "Ada" } as RecordJson]);

  const stop = vi.spyOn(engine, "stop");
  stopEverySync();
  expect(stop).toHaveBeenCalledOnce();
  stopEverySync();
  expect(stop).toHaveBeenCalledOnce();

  await wipe(true);
  // Say the answer to a request sent before signing out arrives now.
  await expect(putRecords("w1", "contact", [{ id: "c2", name: "Grace" } as RecordJson])).rejects.toThrow();
  await expect(setMeta("w1:cursor", 5)).rejects.toThrow();
  const names = (await indexedDB.databases()).map((d) => d.name);
  expect(names).not.toContain("onhold-offline");
});
