import assert from "node:assert/strict";
import { mkdtemp, readFile, readdir, rename, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";

import { test, vi } from "vite-plus/test";

import { writeChanges } from "../src/util/write.ts";

vi.mock(import("node:fs/promises"), async (original) => {
  const fs = await original();
  return { ...fs, rename: vi.fn(fs.rename) };
});

test("reports the write error and each failed restore while recovering other files", async () => {
  const dir = await mkdtemp(join(tmpdir(), "tw-migrate-rollback-"));
  try {
    const paths = ["first.css", "second.css", "third.css"].map((name) => join(dir, name));
    await Promise.all(paths.map((path) => writeFile(path, "original")));
    const realRename = vi.mocked(rename).getMockImplementation()!;
    const writeError = new Error("commit failed");
    const backups: string[] = [];
    vi.mocked(rename).mockImplementation(async (from, to) => {
      if (to === paths[0] && !String(from).includes(".tw-migrate-backup-")) throw writeError;
      if (to === paths[1] || to === paths[2]) {
        backups.push(String(from));
        throw new Error("restore failed");
      }
      return realRename(from, to);
    });
    await assert.rejects(
      writeChanges(
        paths.map((path) => ({ path, before: "original", source: "changed" })),
        [],
      ),
      (error) => {
        assert.ok(error instanceof Error);
        assert.equal(error.cause, writeError);
        assert.match(error.message, /commit failed/);
        assert.match(error.message, /restore failed/);
        assert.equal(backups.length, 2);
        for (const path of [...paths.slice(1), ...backups]) assert.ok(error.message.includes(path));
        return true;
      },
    );
    assert.deepEqual(
      (await readdir(dir)).sort(),
      [basename(paths[0]), ...backups.map((path) => basename(path))].sort(),
    );
    for (const path of [paths[0], ...backups])
      assert.equal(await readFile(path, "utf8"), "original");
  } finally {
    vi.mocked(rename).mockReset();
    await rm(dir, { recursive: true, force: true });
  }
});
