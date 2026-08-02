import { spawn } from "node:child_process";
import { closeSync, openSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";

import { availablePort, terminateTree, waitForHttpOk } from "./shared.ts";
import type { RunningServer } from "./types.ts";

const require = createRequire(import.meta.url);

export function registryConfig({
  storage,
  allowPublish,
}: {
  storage: string;
  allowPublish: boolean;
}): string {
  const publish = allowPublish ? "$all" : "nobody";
  return `storage: ${JSON.stringify(storage)}
auth:
  htpasswd:
    file: ${JSON.stringify(join(storage, "htpasswd"))}
    max_users: ${allowPublish ? 1 : -1}
uplinks:
  npmjs:
    url: https://registry.npmjs.org/
packages:
  'tw-migrate':
    access: $all
    publish: ${publish}
    unpublish: nobody
    proxy: false
  'tw-migrate-*':
    access: $all
    publish: ${publish}
    unpublish: nobody
    proxy: false
  '@*/*':
    access: $all
    publish: nobody
    unpublish: nobody
    proxy: npmjs
  '**':
    access: $all
    publish: nobody
    unpublish: nobody
    proxy: npmjs
log: { type: stdout, format: pretty, level: http }
`;
}

export async function startRegistry({
  root,
  artifactRoot,
  allowPublish,
}: {
  root: string;
  artifactRoot: string;
  allowPublish: boolean;
}): Promise<RunningServer> {
  const timeoutMs = 15_000;
  const verdaccioBin = join(dirname(require.resolve("verdaccio/package.json")), "bin", "verdaccio");
  await Promise.all([mkdir(root, { recursive: true }), mkdir(artifactRoot, { recursive: true })]);
  const storage = join(root, "storage");
  const configPath = join(root, allowPublish ? "bootstrap.yaml" : "sealed.yaml");
  const logPath = join(
    artifactRoot,
    allowPublish ? "registry-bootstrap.log" : "registry-install.log",
  );
  await mkdir(storage, { recursive: true });
  await writeFile(configPath, registryConfig({ storage, allowPublish }));
  const port = await availablePort();
  const url = `http://127.0.0.1:${port}`;
  const log = openSync(logPath, "a");
  const child = spawn(
    process.execPath,
    [verdaccioBin, "--config", configPath, "--listen", `127.0.0.1:${port}`],
    {
      detached: process.platform !== "win32",
      stdio: ["ignore", log, log],
      windowsHide: true,
    },
  );

  try {
    await waitForHttpOk(`${url}/-/ping`, child, timeoutMs, "registry");
  } catch (error) {
    await terminateTree(child);
    closeSync(log);
    const output = await readFile(logPath, "utf8").catch(() => "");
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`${message}\nregistry log: ${logPath}\n${output}`);
  }

  let stopped = false;
  return {
    url,
    async stop() {
      if (stopped) return;
      stopped = true;
      await terminateTree(child);
      closeSync(log);
    },
  };
}
