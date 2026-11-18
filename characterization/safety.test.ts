import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "bun:test";

import { assertSafeDatabase, liveDatabasePaths } from "./src/paths.ts";

test("the suite refuses the live database paths", () => {
  const env = {
    HOME: "/home/mat",
    XDG_DATA_HOME: "/home/mat/.local/share",
  };
  const live = liveDatabasePaths(env);
  expect(live).toContain("/home/mat/Library/Application Support/audetic/audetic.db");
  expect(live).toContain("/home/mat/.local/share/audetic/audetic.db");
  for (const path of live) {
    expect(() => assertSafeDatabase(path, env)).toThrow(/live Audetic database/);
  }

  const root = mkdtempSync(join(tmpdir(), "audetic-safe-"));
  expect(() => assertSafeDatabase(join(root, "audetic.db"), env)).not.toThrow();
});
