import { chmod, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { basename, dirname, join } from "node:path";

import { errorCode, errorMessage } from "./shared.ts";
import type { SourceFile } from "../types.ts";

export async function verifySnapshots(snapshots: Map<string, string>): Promise<void> {
  const entries = [...snapshots].sort(([left], [right]) => left.localeCompare(right));
  // Reads run concurrently, but failures are reported in sorted path order so
  // the surfaced integrity error stays deterministic.
  const reads = await Promise.allSettled(entries.map(([path]) => readFile(path, "utf8")));
  for (const [index, [path, before]] of entries.entries()) {
    const read = reads[index];
    if (read.status === "rejected") {
      const error: unknown = read.reason;
      const detail = errorCode(error) ?? errorMessage(error);
      throw new Error(`Source changed after planning: ${path} (${detail})`);
    }
    if (read.value !== before) throw new Error(`Source changed after planning: ${path}`);
  }
}

export async function writeChanges(
  changes: (SourceFile & { before: string })[],
  deletions: { path: string; before: string }[],
): Promise<void> {
  const token = `${process.pid}-${Date.now()}`;
  const staged = changes.map((change, index): [string, SourceFile] => [
    join(dirname(change.path), `.${basename(change.path)}.tw-migrate-${token}-${index}`),
    change,
  ]);
  const backups = [...changes, ...deletions].map((change, index): [string, string] => [
    join(dirname(change.path), `.${basename(change.path)}.tw-migrate-backup-${token}-${index}`),
    change.path,
  ]);
  const backedUp: [string, string][] = [];
  let succeeded = false;
  try {
    const stagingResults = await Promise.allSettled(
      staged.map(async ([temporaryPath, change]) => {
        const { mode } = await stat(change.path);
        await writeFile(temporaryPath, change.source);
        await chmod(temporaryPath, mode & 0o777);
      }),
    );
    for (const result of stagingResults) {
      if (result.status === "rejected") throw result.reason;
    }
    for (const [backupPath, originalPath] of backups) {
      await rename(originalPath, backupPath);
      backedUp.push([backupPath, originalPath]);
    }
    for (const [temporaryPath, change] of staged) await rename(temporaryPath, change.path);
    succeeded = true;
  } finally {
    if (succeeded) {
      await Promise.all(backups.map(([backupPath]) => rm(backupPath, { force: true })));
    } else {
      for (const [backupPath, originalPath] of backedUp.reverse()) {
        try {
          await rename(backupPath, originalPath);
        } catch {}
      }
    }
    await Promise.all(staged.map(([temporaryPath]) => rm(temporaryPath, { force: true })));
  }
}
