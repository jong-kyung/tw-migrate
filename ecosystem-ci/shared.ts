import { spawn, spawnSync } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { hash } from "node:crypto";
import { once } from "node:events";
import { closeSync, openSync } from "node:fs";
import { readFile } from "node:fs/promises";
import net from "node:net";
import { isAbsolute, relative } from "node:path";

import type { RunningServer } from "./types.ts";

// Windows resolves bare `npm`/`pnpm`/`npx`/`corepack` to a `.cmd` shim that
// spawn() can only execute through a shell.
export function platformCommand(name: string): string {
  return process.platform === "win32" ? `${name}.cmd` : name;
}

export function inside(path: string, root: string): boolean {
  const rel = relative(root, path);
  return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}

export async function availablePort(): Promise<number> {
  return new Promise((resolvePort, reject) => {
    const server = net.createServer();
    server.unref();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (address === null || typeof address === "string") {
        server.close(() => reject(new Error("listening socket reported no numeric port")));
        return;
      }
      const { port } = address;
      server.close((error) => (error ? reject(error) : resolvePort(port)));
    });
  });
}

export async function sha256(path: string): Promise<string> {
  return hash("sha256", await readFile(path), "hex");
}

export async function terminateTree(child: ChildProcess): Promise<void> {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise<void>((resolveExit) => child.once("exit", () => resolveExit()));
  const pid = child.pid as number;
  if (process.platform === "win32") {
    spawnSync("taskkill.exe", ["/pid", String(pid), "/t", "/f"], { windowsHide: true });
  } else {
    try {
      process.kill(-pid, "SIGTERM");
    } catch {}
  }
  const stopped = await Promise.race([
    exited.then(() => true),
    new Promise<boolean>((resolveWait) => setTimeout(() => resolveWait(false), 3_000)),
  ]);
  if (!stopped && process.platform !== "win32") {
    try {
      process.kill(-pid, "SIGKILL");
    } catch {}
  }
  if (!stopped) {
    const forced: boolean = await Promise.race([
      exited.then(() => true),
      new Promise<boolean>((resolveWait) => setTimeout(() => resolveWait(false), 3_000)),
    ]);
    if (!forced) throw new Error(`child process ${pid} did not exit`);
  }
}

export async function waitForChild(
  child: ChildProcess,
  { timeoutMs }: { timeoutMs: number },
): Promise<{ code: number | null; signal: NodeJS.Signals | null }> {
  try {
    const [code = null, signal = null] = await once(child, "exit", {
      signal: AbortSignal.timeout(timeoutMs),
    });
    return { code, signal };
  } catch (error) {
    if (!(error instanceof Error && error.name === "AbortError")) throw error;
    // terminateTree is internally bounded (two 3-second waits) and always
    // settles, so no extra teardown timeout is needed here.
    await terminateTree(child);
    throw new Error(`command timed out after ${timeoutMs}ms`);
  }
}

export async function run(
  command: string,
  args: string[],
  {
    cwd,
    logPath,
    timeoutMs = 180_000,
    env,
  }: { cwd: string; logPath: string; timeoutMs?: number; env?: NodeJS.ProcessEnv },
): Promise<void> {
  const log = openSync(logPath, "a");
  const child = spawn(command, args, {
    cwd,
    detached: process.platform !== "win32",
    env,
    shell: command.endsWith(".cmd"),
    windowsHide: true,
    stdio: ["ignore", log, log],
  });
  const result = await waitForChild(child, { timeoutMs }).finally(() => closeSync(log));
  if (result.code !== 0)
    throw new Error(
      `${command} ${args.join(" ")} failed (${result.signal ?? result.code}); see ${logPath}`,
    );
}

export async function waitForHttpOk(
  url: string,
  child: ChildProcess,
  timeoutMs: number,
  description: string,
): Promise<void> {
  let launchError: Error | undefined;
  child.once("error", (error) => {
    launchError = error;
  });
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (launchError) throw launchError;
    if (child.exitCode !== null) throw new Error(`${description} exited with ${child.exitCode}`);
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(1_000) });
      if (response.ok) return;
    } catch {}
    await new Promise((resolveWait) => setTimeout(resolveWait, 200));
  }
  throw new Error(`${description} readiness timed out`);
}

/// Spawn an HTTP server child with output appended to `logPath`, wait until
/// `readyUrl` answers OK, and return the server with an idempotent stop that
/// terminates the process tree and closes the log. Readiness failure tears
/// the child down before rethrowing.
export async function startHttpServerProcess(
  command: string,
  args: string[],
  {
    cwd,
    env,
    logPath,
    url,
    readyUrl = url,
    timeoutMs,
    description,
  }: {
    cwd?: string;
    env?: NodeJS.ProcessEnv;
    logPath: string;
    url: string;
    readyUrl?: string;
    timeoutMs: number;
    description: string;
  },
): Promise<RunningServer> {
  const log = openSync(logPath, "a");
  const child = spawn(command, args, {
    cwd,
    detached: process.platform !== "win32",
    env,
    shell: command.endsWith(".cmd"),
    windowsHide: true,
    stdio: ["ignore", log, log],
  });
  try {
    await waitForHttpOk(readyUrl, child, timeoutMs, description);
  } catch (error) {
    await terminateTree(child);
    closeSync(log);
    throw error;
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
