import { spawn, spawnSync } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { createHash } from "node:crypto";
import { closeSync, openSync } from "node:fs";
import { readFile } from "node:fs/promises";
import net from "node:net";
import { isAbsolute, relative } from "node:path";

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
  return createHash("sha256")
    .update(await readFile(path))
    .digest("hex");
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
  {
    timeoutMs,
    teardownTimeoutMs = 7_000,
    terminate = terminateTree,
  }: {
    timeoutMs: number;
    teardownTimeoutMs?: number;
    terminate?: (child: ChildProcess) => Promise<void>;
  },
): Promise<{ code: number | null; signal: NodeJS.Signals | null }> {
  const timedOut = Symbol("timed out");
  let timer: NodeJS.Timeout | undefined;
  const outcome = new Promise<{
    error?: Error;
    code?: number | null;
    signal?: NodeJS.Signals | null;
  }>((resolveRun) => {
    child.once("error", (error) => resolveRun({ error }));
    child.once("exit", (code, signal) => resolveRun({ code, signal }));
  });
  const result = await Promise.race([
    outcome,
    new Promise<typeof timedOut>((resolveTimeout) => {
      timer = setTimeout(() => resolveTimeout(timedOut), timeoutMs);
    }),
  ]);
  clearTimeout(timer);
  if (result !== timedOut) {
    if (result.error) throw result.error;
    return { code: result.code ?? null, signal: result.signal ?? null };
  }

  let teardownTimer: NodeJS.Timeout | undefined;
  try {
    const teardown = await Promise.race([
      Promise.resolve()
        .then(() => terminate(child))
        .then(
          () => null,
          (error) => error,
        ),
      new Promise<Error>((resolveTimeout) => {
        teardownTimer = setTimeout(
          () =>
            resolveTimeout(new Error(`process teardown timed out after ${teardownTimeoutMs}ms`)),
          teardownTimeoutMs,
        );
      }),
    ]);
    if (teardown)
      throw new Error(
        `command timed out after ${timeoutMs}ms and teardown failed: ${teardown.message}`,
        { cause: teardown },
      );
    throw new Error(`command timed out after ${timeoutMs}ms`);
  } finally {
    clearTimeout(teardownTimer);
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
