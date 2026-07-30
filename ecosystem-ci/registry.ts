import { spawn } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { closeSync, openSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";

import { availablePort } from "./shared.ts";

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

async function waitForRegistry(url: string, child: ChildProcess, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`registry exited with status ${child.exitCode}`);
    try {
      const response = await fetch(`${url}/-/ping`, { signal: AbortSignal.timeout(500) });
      if (response.ok) return;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`registry did not start within ${timeoutMs}ms`);
}

async function terminate(child: ChildProcess, timeoutMs = 5_000): Promise<void> {
  if (child.exitCode !== null) return;
  child.kill("SIGTERM");
  const exited = new Promise<void>((resolve) => child.once("exit", () => resolve()));
  const timedOut = await Promise.race([
    exited.then(() => false),
    new Promise<boolean>((resolve) => setTimeout(() => resolve(true), timeoutMs)),
  ]);
  if (timedOut && child.exitCode === null) {
    child.kill("SIGKILL");
    await exited;
  }
}

export interface Registry {
  url: string;
  logPath: string;
  stop: () => Promise<void>;
}

export async function startRegistry({
  root,
  artifactRoot,
  allowPublish,
  timeoutMs = 15_000,
}: {
  root: string;
  artifactRoot: string;
  allowPublish: boolean;
  timeoutMs?: number;
}): Promise<Registry> {
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
      stdio: ["ignore", log, log],
      windowsHide: true,
    },
  );

  try {
    await waitForRegistry(url, child, timeoutMs);
  } catch (error) {
    await terminate(child);
    closeSync(log);
    const output = await readFile(logPath, "utf8").catch(() => "");
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`${message}\nregistry log: ${logPath}\n${output}`);
  }

  let stopped = false;
  return {
    url,
    logPath,
    async stop() {
      if (stopped) return;
      stopped = true;
      await terminate(child);
      closeSync(log);
    },
  };
}
