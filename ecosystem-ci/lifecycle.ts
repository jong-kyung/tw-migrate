import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { closeSync, openSync } from "node:fs";
import {
  appendFile,
  cp,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  realpath,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, extname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  assertInstalledLayout,
  publishPackages,
  stagePackages,
  validateProvenance,
} from "./packages.ts";
import { startRegistry } from "./registry.ts";
import { assertOracle, captureAll, maxCaptureAttempts } from "./oracle.ts";
import { availablePort, inside, platformCommand, sha256 } from "./shared.ts";
import type {
  CaptureArtifact,
  CaptureSet,
  ControlledProject,
  ExternalProject,
  InstalledLayout,
  MigrationReport,
  PhaseLedger,
  Probe,
  ProbedProject,
  Provenance,
  RunningServer,
} from "./types.ts";

type Browser = import("playwright").Browser;

export interface LifecycleResult {
  baseline: CaptureSet;
  post: CaptureSet;
  ledger: PhaseLedger;
  first?: MigrationReport;
  second?: MigrationReport;
}

type FileDigests = Record<string, string>;

interface LifecycleContext {
  ledger: PhaseLedger;
  mark: (phase: string, files?: string[]) => Promise<void>;
  diagnostic: (phase: string, name: string, attempt: number) => CaptureArtifact;
  runRoot: string;
}

interface ExpectedCase {
  first: MigrationReport;
  changedFiles: Record<string, string>;
  source: string;
}

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const caches = [".next", "dist", "node_modules/.vite"];
const migrationSourceExtensions = new Set([
  ".js",
  ".jsx",
  ".ts",
  ".tsx",
  ".html",
  ".vue",
  ".css",
  ".scss",
  ".sass",
  ".less",
]);
const generatedDirectories = new Set([
  "node_modules",
  ".next",
  "dist",
  "build",
  "out",
  "coverage",
  ".cache",
  ".vite",
]);
export const lifecycleTimeoutMs = 4 * 60_000;
export const externalLifecycleTimeoutMs = 10 * 60_000;

// Windows runners expose TEMP as an 8.3 short path, which crashes libuv
// fs-event watchers (dev servers) with a prefix assertion; watch long paths.
export async function temporaryDirectory(prefix: string): Promise<string> {
  return mkdtemp(join(await realpath(tmpdir()), prefix));
}

export async function artifactAllowlist(
  root: string,
  entries: string[],
  maxBytes = 100 * 1024 * 1024,
): Promise<string[]> {
  root = resolve(root);
  const canonicalRoot = await realpath(root);
  const paths: string[] = [];
  let bytes = 0;
  for (const entry of entries) {
    const path = resolve(root, entry);
    if (!inside(path, root)) throw new Error(`artifact path escapes root: ${entry}`);
    const stat = await lstat(path);
    if (!stat.isFile() || stat.isSymbolicLink())
      throw new Error(`artifact is not a regular file: ${entry}`);
    if (!inside(await realpath(path), canonicalRoot))
      throw new Error(`artifact path escapes root through a symlink: ${entry}`);
    bytes += stat.size;
    if (bytes > maxBytes) throw new Error(`artifact allowlist exceeds ${maxBytes} bytes`);
    paths.push(path);
  }
  return paths;
}

export function assertExpectedChangedFiles(
  changedFiles: string[],
  expectedFiles: Record<string, string>,
  actualFiles: Record<string, string>,
): void {
  assert.deepEqual(
    Object.keys(expectedFiles).sort(),
    [...changedFiles].sort(),
    "exact-file expectations cover changedFiles",
  );
  assert.deepEqual(
    Object.keys(actualFiles).sort(),
    [...changedFiles].sort(),
    "exact changedFiles were read",
  );
  for (const path of changedFiles) {
    assert.deepEqual(
      Buffer.from(actualFiles[path] ?? ""),
      Buffer.from(expectedFiles[path] ?? ""),
      `exact post-migration bytes: ${path}`,
    );
  }
}

// Withholding authored styles must keep a Vue SFC renderable: only the style
// block contents are replaced there, while plain stylesheet sources are
// emptied whole as before. The comment keeps each block non-empty because
// @vue/compiler-sfc drops empty blocks from the descriptor, which would strip
// the `$style` injection a `<style module>` template binding depends on.
export function withheldStyles(project: ControlledProject, source: string): string {
  if (project.runtime !== "vue-vite") return "";
  return source.replace(/(<style\b[^>]*>)[\s\S]*?(<\/style>)/g, "$1/* withheld */$2");
}

export function assertMigrationContract({
  first,
  expectedFirst,
  actualSource,
  expectedSource,
  second,
  treeBeforeSecond,
  treeAfterSecond,
}: {
  first: MigrationReport;
  expectedFirst: MigrationReport;
  actualSource: string | null;
  expectedSource: string;
  second: MigrationReport;
  treeBeforeSecond: FileDigests;
  treeAfterSecond: FileDigests;
}): void {
  assert.deepEqual(first, expectedFirst, "exact first MigrationReport");
  assert.equal(actualSource, expectedSource, "exact migration-owned source");
  assert.deepEqual(second.changedFiles, [], "second migration changedFiles");
  assert.equal(second.diff, "", "second migration diff");
  assert.deepEqual(treeAfterSecond, treeBeforeSecond, "source-scoped tree after second migration");
}

async function terminateTree(child: ChildProcess): Promise<void> {
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

function packageManagerInvocation(
  project: ExternalProject,
  args: string[],
): { command: string; args: string[] } {
  const separator = project.packageManager.indexOf("@");
  const manager = project.packageManager.slice(0, separator);
  const version = project.packageManager.slice(separator + 1);
  if (manager === "npm") {
    return { command: platformCommand("npx"), args: ["--yes", `npm@${version}`, ...args] };
  }
  return { command: platformCommand("corepack"), args: [`${manager}@${version}`, ...args] };
}

async function waitForServer(
  child: ChildProcess,
  log: number,
  port: number,
  description: string,
  timeoutMs: number,
): Promise<RunningServer> {
  let launchError: Error | undefined;
  child.once("error", (error) => {
    launchError = error;
  });
  const url = `http://127.0.0.1:${port}`;
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      if (launchError) throw launchError;
      if (child.exitCode !== null) throw new Error(`${description} exited with ${child.exitCode}`);
      try {
        const response = await fetch(url, { signal: AbortSignal.timeout(1_000) });
        if (response.ok)
          return {
            url,
            async stop() {
              await terminateTree(child);
              closeSync(log);
            },
          };
      } catch {}
      await new Promise((resolveWait) => setTimeout(resolveWait, 200));
    }
    throw new Error(`${description} readiness timed out`);
  } catch (error) {
    await terminateTree(child);
    closeSync(log);
    throw error;
  }
}

async function startServer(
  project: ProbedProject,
  cwd: string,
  artifactRoot: string,
  phase: string,
  mode: "dev" | "preview" = "dev",
): Promise<RunningServer> {
  const port = await availablePort();
  const npm = platformCommand("npm");
  const args =
    mode === "preview"
      ? ["run", "preview", "--", "--host", "127.0.0.1", "--port", String(port), "--strictPort"]
      : "runtime" in project && project.runtime === "next"
        ? ["run", "dev", "--", "--hostname", "127.0.0.1", "--port", String(port)]
        : ["run", "dev", "--", "--host", "127.0.0.1", "--port", String(port), "--strictPort"];
  const logPath = join(artifactRoot, `${phase}-server.log`);
  const log = openSync(logPath, "a");
  const child = spawn(npm, args, {
    cwd,
    detached: process.platform !== "win32",
    shell: npm.endsWith(".cmd"),
    windowsHide: true,
    stdio: ["ignore", log, log],
  });
  return waitForServer(child, log, port, `${phase} server`, 60_000);
}

export function externalEnvironment(): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { CI: "true" };
  for (const key of [
    "PATH",
    "HOME",
    "USERPROFILE",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "LOCALAPPDATA",
    "APPDATA",
  ]) {
    if (process.env[key] !== undefined) env[key] = process.env[key];
  }
  return env;
}

async function startExternalServer(
  project: ExternalProject,
  cwd: string,
  artifactRoot: string,
  phase: string,
): Promise<RunningServer> {
  const port = await availablePort();
  const serverArgs =
    project.server === "next"
      ? ["--hostname", "127.0.0.1", "--port", String(port)]
      : ["--host", "127.0.0.1", "--port", String(port), "--strictPort"];
  const separator = project.packageManager.startsWith("npm@") ? ["--"] : [];
  const invocation = packageManagerInvocation(project, [
    ...project.start,
    ...separator,
    ...serverArgs,
  ]);
  const logPath = join(artifactRoot, `${phase}-server.log`);
  const log = openSync(logPath, "a");
  const child = spawn(invocation.command, invocation.args, {
    cwd,
    detached: process.platform !== "win32",
    env: externalEnvironment(),
    shell: invocation.command.endsWith(".cmd"),
    windowsHide: true,
    stdio: ["ignore", log, log],
  });
  return waitForServer(child, log, port, `${phase} external server`, 90_000);
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

async function run(
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

async function checkoutExternalProject(
  project: ExternalProject,
  runRoot: string,
  artifactRoot: string,
): Promise<string> {
  const projectRoot = join(runRoot, "external");
  await mkdir(projectRoot, { recursive: true });
  const logPath = join(artifactRoot, "checkout.log");
  const env = externalEnvironment();
  await run("git", ["init"], { cwd: projectRoot, logPath, env });
  await run("git", ["remote", "add", "origin", project.repository], {
    cwd: projectRoot,
    logPath,
    env,
  });
  await run("git", ["fetch", "--depth=1", "origin", project.revision], {
    cwd: projectRoot,
    logPath,
    timeoutMs: 300_000,
    env,
  });
  await run("git", ["checkout", "--detach", "FETCH_HEAD"], { cwd: projectRoot, logPath, env });
  const head = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: projectRoot,
    encoding: "utf8",
    env,
    windowsHide: true,
  });
  if (head.status !== 0 || head.stdout.trim() !== project.revision) {
    throw new Error(`external checkout HEAD did not match ${project.revision}`);
  }
  return projectRoot;
}

function trackedCheckoutChanges(root: string): { status: string; path: string }[] {
  const status = spawnSync("git", ["status", "--porcelain=v1", "-z", "--untracked-files=no"], {
    cwd: root,
    encoding: "utf8",
    env: externalEnvironment(),
    windowsHide: true,
  });
  if (status.status !== 0)
    throw new Error(`could not verify external checkout: ${status.stderr.trim()}`);
  return status.stdout
    .split("\0")
    .filter(Boolean)
    .map((entry) => ({ status: entry.slice(0, 2), path: entry.slice(3) }));
}

function assertTrackedCheckoutClean(root: string, phase: string): void {
  const changes = trackedCheckoutChanges(root);
  if (changes.length > 0) {
    throw new Error(
      `external checkout changed tracked files during ${phase}: ${changes.map(({ status, path }) => `${status} ${path}`).join(", ")}`,
    );
  }
}

function trackedCheckoutDiff(root: string): Buffer {
  const diff = spawnSync(
    "git",
    ["diff", "HEAD", "--binary", "--no-ext-diff", "--no-textconv", "--"],
    {
      cwd: root,
      env: externalEnvironment(),
      maxBuffer: 100 * 1024 * 1024,
      windowsHide: true,
    },
  );
  if (diff.status !== 0)
    throw new Error(`could not diff external checkout: ${String(diff.stderr).trim()}`);
  return diff.stdout;
}

async function snapshotRuntimeWrites(
  root: string,
  paths: string[],
): Promise<Record<string, Buffer>> {
  const canonicalRoot = await realpath(root);
  return Object.fromEntries(
    await Promise.all(
      paths.map(async (path) => [
        path,
        await readFile(await checkedMigrationPath(root, canonicalRoot, path)),
      ]),
    ),
  );
}

export async function restoreRuntimeWrites(
  root: string,
  originals: Record<string, Buffer>,
  phase: string,
  expectedDiff: Buffer = Buffer.alloc(0),
): Promise<void> {
  const canonicalRoot = await realpath(root);
  for (const [path, contents] of Object.entries(originals)) {
    await writeFile(await checkedMigrationPath(root, canonicalRoot, path), contents);
  }
  if (!trackedCheckoutDiff(root).equals(expectedDiff)) {
    const changes = trackedCheckoutChanges(root);
    throw new Error(
      `external checkout changed unreviewed tracked files during ${phase}: ${changes.map(({ status, path }) => `${status} ${path}`).join(", ")}`,
    );
  }
}

async function checkedProjectDirectory(root: string, relativePath: string): Promise<string> {
  const path = resolve(root, relativePath);
  if (!inside(path, root)) throw new Error(`project directory escapes checkout: ${relativePath}`);
  const stat = await lstat(path);
  if (!stat.isDirectory() || stat.isSymbolicLink())
    throw new Error(`project path is not a regular directory: ${relativePath}`);
  if (!inside(await realpath(path), await realpath(root)))
    throw new Error(`project directory escapes checkout through a symlink: ${relativePath}`);
  return path;
}

async function checkedMigrationPath(
  root: string,
  canonicalRoot: string,
  relativePath: string,
): Promise<string> {
  const path = resolve(root, relativePath);
  if (!inside(path, root)) throw new Error(`migration-owned path escapes project: ${relativePath}`);
  const stat = await lstat(path);
  if (!stat.isFile() || stat.isSymbolicLink())
    throw new Error(`migration-owned path is not a regular file: ${relativePath}`);
  if (!inside(await realpath(path), canonicalRoot))
    throw new Error(`migration-owned path escapes project through a symlink: ${relativePath}`);
  return path;
}

async function readMigrationPaths(root: string, paths: string[]): Promise<Record<string, string>> {
  root = resolve(root);
  const canonicalRoot = await realpath(root);
  const result: Record<string, string> = {};
  for (const relativePath of [...paths].sort()) {
    result[relativePath] = await readFile(
      await checkedMigrationPath(root, canonicalRoot, relativePath),
      "utf8",
    );
  }
  return result;
}

export async function clearGeneratedCaches(root: string): Promise<void> {
  await Promise.all(
    caches.map((path) =>
      rm(join(root, path), {
        recursive: true,
        force: true,
        maxRetries: 5,
        retryDelay: 100,
      }),
    ),
  );
}

export async function snapshotMigrationSources(root: string): Promise<FileDigests> {
  root = resolve(root);
  const canonicalRoot = await realpath(root);
  const result: FileDigests = {};
  const walk = async (directory: string): Promise<void> => {
    const stat = await lstat(directory);
    if (!stat.isDirectory() || stat.isSymbolicLink())
      throw new Error(
        `migration source path is not a regular directory: ${relative(root, directory)}`,
      );
    if (!inside(await realpath(directory), canonicalRoot))
      throw new Error(
        `migration source path escapes project through a symlink: ${relative(root, directory)}`,
      );
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      if (generatedDirectories.has(entry.name)) continue;
      const path = resolve(directory, entry.name);
      if (!inside(path, root)) throw new Error(`migration source path escapes project: ${path}`);
      if (entry.isSymbolicLink())
        throw new Error(`migration source path is not regular: ${relative(root, path)}`);
      if (entry.isDirectory()) {
        await walk(path);
      } else if (migrationSourceExtensions.has(extname(entry.name).toLowerCase())) {
        const relativePath = relative(root, path);
        const checked = await checkedMigrationPath(root, canonicalRoot, relativePath);
        result[relativePath] = await sha256(checked);
      }
    }
  };
  await walk(root);
  return Object.fromEntries(
    Object.entries(result).sort(([left], [right]) => left.localeCompare(right)),
  );
}

async function prepareDriver(
  project: ControlledProject,
  runRoot: string,
  packageArtifactRoot: string,
  artifactRoot: string,
): Promise<{ driverRoot: string; installed: InstalledLayout }> {
  await mkdir(packageArtifactRoot, { recursive: true });
  const provenancePath = join(packageArtifactRoot, "provenance.json");
  let provenance: Provenance;
  try {
    provenance = JSON.parse(await readFile(provenancePath, "utf8")) as Provenance;
    const git = spawnSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8" });
    if (git.status !== 0) throw new Error("could not verify package provenance commit");
    await validateProvenance(provenance, {
      artifactRoot: packageArtifactRoot,
      expectedCommit: git.stdout.trim(),
    });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    provenance = await stagePackages({ repoRoot, artifactRoot: packageArtifactRoot });
  }

  const registryRoot = join(runRoot, "registry");
  const bootstrapRegistry = await startRegistry({
    root: registryRoot,
    artifactRoot,
    allowPublish: true,
  });
  try {
    await publishPackages(
      provenance,
      packageArtifactRoot,
      bootstrapRegistry.url,
      join(artifactRoot, "publish.log"),
    );
  } finally {
    await bootstrapRegistry.stop();
  }
  const sealedRegistry = await startRegistry({
    root: registryRoot,
    artifactRoot,
    allowPublish: false,
  });
  const fixture = join(
    repoRoot,
    "ecosystem-ci",
    "fixtures",
    "controlled",
    project.runtime,
    project.style,
  );
  const driverRoot = join(runRoot, "driver");
  try {
    await cp(fixture, driverRoot, { recursive: true });
    const manifestPath = join(driverRoot, "package.json");
    const manifest = JSON.parse(await readFile(manifestPath, "utf8")) as {
      dependencies: Record<string, string>;
    };
    manifest.dependencies["tw-migrate"] = provenance.packages.root.version;
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
    await run(
      platformCommand("npm"),
      [
        "install",
        "--registry",
        sealedRegistry.url,
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        "--fetch-retries=0",
      ],
      { cwd: driverRoot, logPath: join(artifactRoot, "install.log") },
    );
  } finally {
    await sealedRegistry.stop();
  }
  const installed = await assertInstalledLayout({
    driverRoot,
    checkoutRoot: repoRoot,
    expected: {
      version: provenance.packages.root.version,
      platform: provenance.platform,
      addonSha256: provenance.addon.sha256,
    },
  });
  return { driverRoot, installed };
}

// The installed layout is owned by the root package manifest, so resolve the
// API and CLI entrypoints from it instead of hardcoding file paths.
async function installedEntrypoint(root: string, entry: "main" | "bin"): Promise<string> {
  const manifest = JSON.parse(await readFile(join(root, "package.json"), "utf8")) as {
    main: string;
    bin: Record<string, string>;
  };
  return join(root, entry === "main" ? manifest.main : manifest.bin["tw-migrate"]);
}

async function readMaybe(path: string): Promise<string | null> {
  return readFile(path, "utf8").catch((error: NodeJS.ErrnoException) =>
    error.code === "ENOENT" ? null : Promise.reject(error),
  );
}

export async function teardownLifecycleServer(
  server: RunningServer | undefined,
  primaryError: unknown,
  recordFailure: (error: unknown) => Promise<void>,
): Promise<void> {
  try {
    await server?.stop();
  } catch (error) {
    if (primaryError) return;
    await recordFailure(error);
    throw error;
  }
}

// Every lifecycle phase captures the same way and writes the same
// `<phase>-computed.json` artifact; only the phase name and server URL vary.
// Binding the probe set here keeps a smoke run from reading `project.probes`
// when its captures must come from the fixture it drives.
function capturePhase(
  browser: Browser,
  probes: Record<string, Probe>,
  artifactRoot: string,
  diagnostic: (phase: string, name: string, attempt: number) => CaptureArtifact,
): (phase: string, baseUrl: string) => Promise<CaptureSet> {
  return async (phase, baseUrl) => {
    const captured = await captureAll(browser, baseUrl, probes, (name, attempt) =>
      diagnostic(phase, name, attempt),
    );
    await writeFile(
      join(artifactRoot, `${phase}-computed.json`),
      `${JSON.stringify(captured, null, 2)}\n`,
    );
    return captured;
  };
}

export function captureAttemptArtifactNames(
  phase: string,
  probe: string,
  attempt: number,
): [string, string] {
  return [
    `${phase}-${probe}-attempt-${attempt}-browser.json`,
    `${phase}-${probe}-attempt-${attempt}.png`,
  ];
}

function captureArtifactNames(project: ProbedProject, phase: string): string[] {
  return [
    `${phase}-computed.json`,
    `${phase}-server.log`,
    ...Object.keys(project.probes).flatMap((probe) =>
      Array.from({ length: maxCaptureAttempts }, (_, index) =>
        captureAttemptArtifactNames(phase, probe, index + 1),
      ).flat(),
    ),
  ];
}

async function existingArtifactNames(artifactRoot: string, names: string[]): Promise<string[]> {
  const existing: string[] = [];
  for (const name of names) {
    if ((await lstat(join(artifactRoot, name)).catch(() => null))?.isFile()) existing.push(name);
  }
  return existing;
}

function caseArtifactNames(project: ProbedProject): string[] {
  return [
    "phase-ledger.json",
    "failure.log",
    "install.log",
    "publish.log",
    "registry-bootstrap.log",
    "registry-install.log",
    "first-report.json",
    "second-report.json",
    "source.diff",
    "baseline-build.log",
    "post-build.log",
    "first-cli.log",
    "second-cli.log",
    "checkout.log",
    "external-install-1.log",
    "external-install-2.log",
    "external-install-3.log",
    "external-install-4.log",
    ...["baseline", "withheld", "utilities-only", "post"].flatMap((phase) =>
      captureArtifactNames(project, phase),
    ),
  ];
}

export async function prepareCaseUpload(
  project: ProbedProject,
  artifactRoot: string,
  uploadRoot: string,
): Promise<string[]> {
  const allowed = new Set(caseArtifactNames(project));
  const ledger = JSON.parse(
    await readFile(join(artifactRoot, "phase-ledger.json"), "utf8"),
  ) as PhaseLedger;
  const declared = new Set(["phase-ledger.json", ...(ledger.failureFiles ?? [])]);
  for (const phase of ledger.phases) for (const file of phase.files) declared.add(file);
  if (ledger.failure) declared.add("failure.log");
  const existing: string[] = [];
  for (const name of declared) {
    if (!allowed.has(name)) throw new Error(`phase ledger declared forbidden artifact: ${name}`);
    if ((await lstat(join(artifactRoot, name)).catch(() => null))?.isFile()) existing.push(name);
  }
  await artifactAllowlist(artifactRoot, existing);
  await rm(uploadRoot, { recursive: true, force: true });
  for (const name of existing) {
    const destination = join(uploadRoot, name);
    await mkdir(dirname(destination), { recursive: true });
    await cp(join(artifactRoot, name), destination);
  }
  return existing;
}

export function temporaryLifecyclePaths(
  projectId: string,
  temporaryRoot: string,
): { artifactRoot: string; packageArtifactRoot: string } {
  return {
    artifactRoot: join(temporaryRoot, "artifacts", projectId),
    packageArtifactRoot: join(temporaryRoot, "packages"),
  };
}

async function executeLifecycle<T>(
  {
    project,
    artifactCase = project as ProbedProject,
    artifactRoot,
    temporaryRoot,
    activeServer,
  }: {
    project: ControlledProject | ExternalProject | { id: string; kind: "smoke" };
    artifactCase?: ProbedProject;
    artifactRoot: string;
    temporaryRoot?: string;
    activeServer: () => RunningServer | undefined;
  },
  body: (context: LifecycleContext) => Promise<T>,
): Promise<T> {
  const ledger: PhaseLedger = { case: project.id, phases: [] };
  const ledgerPath = join(artifactRoot, "phase-ledger.json");
  const mark = async (phase: string, files: string[] = []) => {
    ledger.phases.push({ phase, files });
    await writeFile(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`);
  };
  const recordFailure = async (error: unknown) => {
    await appendFile(
      join(artifactRoot, "failure.log"),
      `${error instanceof Error ? (error.stack ?? error.message) : String(error)}\n`,
    ).catch(() => {});
    ledger.failure = error instanceof Error ? error.message : String(error);
    ledger.failureFiles = [];
    for (const name of caseArtifactNames(artifactCase)) {
      if ((await lstat(join(artifactRoot, name)).catch(() => null))?.isFile())
        ledger.failureFiles.push(name);
    }
    await writeFile(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`).catch(() => {});
  };
  const diagnostic = (phase: string, name: string, attempt: number): CaptureArtifact => {
    const [browserJson, screenshot] = captureAttemptArtifactNames(phase, name, attempt);
    return {
      screenshot: join(artifactRoot, screenshot),
      writeDiagnostics: (value: unknown) =>
        writeFile(join(artifactRoot, browserJson), `${JSON.stringify(value, null, 2)}\n`),
    };
  };
  await mark("initialized");
  // Short prefix: pnpm's peer-suffixed store directories push Windows module
  // paths toward the 260-character limit, and the case id is already in the
  // artifact roots and ledger.
  const runRoot = await temporaryDirectory("twm-");
  let result: T | undefined;
  let succeeded = false;
  let primaryError: unknown;
  try {
    result = await body({ ledger, mark, diagnostic, runRoot });
    succeeded = true;
  } catch (error) {
    primaryError = error;
    await recordFailure(error);
  }

  // Teardown steps land in the ledger so a post-completion hang is attributable from artifacts.
  const teardownMark = async (step: string) => {
    ledger.teardown = [...(ledger.teardown ?? []), { step, at: new Date().toISOString() }];
    await writeFile(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`).catch(() => {});
  };
  let teardownError: unknown;
  await teardownMark("server-stop-started");
  try {
    await teardownLifecycleServer(activeServer(), primaryError, recordFailure);
  } catch (error) {
    teardownError = error;
  }
  await teardownMark("server-stopped");
  // Orphaned dev-server processes can keep recreating files under runRoot on
  // Windows, so recursive rm never converges; bound it and leave the OS temp
  // directory behind rather than hanging the merge gate on cleanup.
  const removed = await Promise.race([
    rm(runRoot, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 }).then(
      () => true,
      () => false,
    ),
    new Promise<boolean>((resolveWait) => setTimeout(resolveWait, 30_000, false)),
  ]);
  await teardownMark(removed ? "run-root-removed" : "run-root-left-behind");
  if (succeeded && !teardownError && temporaryRoot)
    await rm(temporaryRoot, { recursive: true, force: true });
  if (teardownError) throw teardownError;
  if (!succeeded) throw primaryError;
  return result as T;
}

export async function runLifecycle({
  browser,
  project,
  artifactRoot,
  packageArtifactRoot = process.env.ECOSYSTEM_PACKAGE_ARTIFACT_ROOT,
}: {
  browser: Browser;
  project: ControlledProject;
  artifactRoot?: string;
  packageArtifactRoot?: string;
}): Promise<LifecycleResult> {
  let temporaryRoot: string | undefined;
  if (!artifactRoot) {
    temporaryRoot = await temporaryDirectory(`tw-migrate-${project.id}-artifacts-`);
    ({ artifactRoot, packageArtifactRoot } = temporaryLifecyclePaths(project.id, temporaryRoot));
  }
  artifactRoot = resolve(artifactRoot);
  packageArtifactRoot = packageArtifactRoot ? resolve(packageArtifactRoot) : artifactRoot;
  await rm(artifactRoot, { recursive: true, force: true });
  await mkdir(artifactRoot, { recursive: true });
  let server: RunningServer | undefined;
  return executeLifecycle(
    { project, artifactRoot, temporaryRoot, activeServer: () => server },
    async ({ ledger, mark, diagnostic, runRoot }) => {
      const capture = capturePhase(browser, project.probes, artifactRoot, diagnostic);
      const { driverRoot, installed } = await prepareDriver(
        project,
        runRoot,
        packageArtifactRoot,
        artifactRoot,
      );
      await mark("installed", [
        "install.log",
        "publish.log",
        "registry-bootstrap.log",
        "registry-install.log",
      ]);

      await mark("baseline-started");
      server = await startServer(project, driverRoot, artifactRoot, "baseline");
      const baseline = await capture("baseline", server.url);
      await mark(
        "baseline",
        await existingArtifactNames(artifactRoot, captureArtifactNames(project, "baseline")),
      );
      await server.stop();
      server = undefined;

      const sourcePath = await checkedMigrationPath(
        driverRoot,
        await realpath(driverRoot),
        project.source.path,
      );
      const authored = await readFile(sourcePath, "utf8");
      assert.ok(authored.includes(project.source.before), "causal witness source token");
      const expected = JSON.parse(
        await readFile(join(driverRoot, "expected.json"), "utf8"),
      ) as ExpectedCase;
      assert.ok(
        expected.first.candidates.includes(project.source.after),
        "causal witness candidate token",
      );
      await writeFile(sourcePath, withheldStyles(project, authored));
      await mark("causal-witness-started");
      server = await startServer(project, driverRoot, artifactRoot, "withheld");
      const withheld = await capture("withheld", server.url);
      await server.stop();
      server = undefined;
      await writeFile(sourcePath, authored);
      await mark(
        "causal-witness",
        await existingArtifactNames(artifactRoot, captureArtifactNames(project, "withheld")),
      );

      await mark("migration-started");
      const module = await import(
        `${pathToFileURL(await installedEntrypoint(installed.root, "main")).href}?case=${Date.now()}`
      );
      const first = await module.migrate({
        cwd: driverRoot,
        styleFile: project.source.path,
        write: true,
      });
      await writeFile(
        join(artifactRoot, "first-report.json"),
        `${JSON.stringify(first, null, 2)}\n`,
      );
      await writeFile(join(artifactRoot, "source.diff"), first.diff);
      const actualSource = await readMaybe(sourcePath);
      const actualChangedFiles = await readMigrationPaths(driverRoot, first.changedFiles);
      assertExpectedChangedFiles(first.changedFiles, expected.changedFiles, actualChangedFiles);
      const treeBeforeSecond = await snapshotMigrationSources(driverRoot);
      const second = await module.migrate({
        cwd: driverRoot,
        styleFile: project.source.path,
        write: true,
      });
      const treeAfterSecond = await snapshotMigrationSources(driverRoot);
      await writeFile(
        join(artifactRoot, "second-report.json"),
        `${JSON.stringify(second, null, 2)}\n`,
      );
      await mark("migration-output", ["first-report.json", "second-report.json", "source.diff"]);
      assertMigrationContract({
        first,
        expectedFirst: expected.first,
        actualSource,
        expectedSource: expected.source,
        second,
        treeBeforeSecond,
        treeAfterSecond,
      });
      await mark("migration");

      await writeFile(sourcePath, withheldStyles(project, actualSource as string));
      await mark("utilities-only-started");
      try {
        await clearGeneratedCaches(driverRoot);
        server = await startServer(project, driverRoot, artifactRoot, "utilities-only");
        const utilitiesOnly = await capture("utilities-only", server.url);
        await mark(
          "utilities-only-captured",
          await existingArtifactNames(
            artifactRoot,
            captureArtifactNames(project, "utilities-only"),
          ),
        );
        assert.deepEqual(
          Object.fromEntries(
            Object.entries(utilitiesOnly).map(([name, value]) => [name, value.elements]),
          ),
          Object.fromEntries(
            Object.entries(baseline).map(([name, value]) => [name, value.elements]),
          ),
          "utilities-only computed capture exactly equals baseline",
        );
      } finally {
        await server?.stop();
        server = undefined;
        // Controlled fixtures are always rewritten, never deleted, so the
        // migrated source read back above is a string.
        await writeFile(sourcePath, actualSource as string);
      }
      await mark("utilities-only");

      await mark("post-started");
      await clearGeneratedCaches(driverRoot);
      server = await startServer(project, driverRoot, artifactRoot, "post");
      const post = await capture("post", server.url);
      await mark(
        "post-captured",
        await existingArtifactNames(artifactRoot, captureArtifactNames(project, "post")),
      );
      assertOracle({ baseline, post, withheld, candidateTokens: expected.first.candidates });
      await mark("complete");
      return { baseline, first, second, post, ledger };
    },
  );
}

export async function runProductionSmoke({
  browser,
  project,
  fixture,
  artifactRoot,
  packageArtifactRoot = process.env.ECOSYSTEM_PACKAGE_ARTIFACT_ROOT,
}: {
  browser: Browser;
  project: { id: string; kind: "smoke" };
  fixture: ControlledProject;
  artifactRoot?: string;
  packageArtifactRoot?: string;
}): Promise<LifecycleResult> {
  let temporaryRoot: string | undefined;
  if (!artifactRoot) {
    temporaryRoot = await temporaryDirectory(`tw-migrate-${project.id}-artifacts-`);
    ({ artifactRoot, packageArtifactRoot } = temporaryLifecyclePaths(project.id, temporaryRoot));
  }
  artifactRoot = resolve(artifactRoot);
  packageArtifactRoot = packageArtifactRoot ? resolve(packageArtifactRoot) : artifactRoot;
  await rm(artifactRoot, { recursive: true, force: true });
  await mkdir(artifactRoot, { recursive: true });
  let server: RunningServer | undefined;
  return executeLifecycle(
    { project, artifactCase: fixture, artifactRoot, temporaryRoot, activeServer: () => server },
    async ({ ledger, mark, diagnostic, runRoot }) => {
      const capture = capturePhase(browser, fixture.probes, artifactRoot, diagnostic);
      const { driverRoot, installed } = await prepareDriver(
        fixture,
        runRoot,
        packageArtifactRoot,
        artifactRoot,
      );
      await mark("installed", [
        "install.log",
        "publish.log",
        "registry-bootstrap.log",
        "registry-install.log",
      ]);
      const npm = platformCommand("npm");

      await mark("baseline-build-started");
      await run(npm, ["run", "build"], {
        cwd: driverRoot,
        logPath: join(artifactRoot, "baseline-build.log"),
      });
      await mark("baseline-build", ["baseline-build.log"]);
      server = await startServer(fixture, driverRoot, artifactRoot, "baseline", "preview");
      const baseline = await capture("baseline", server.url);
      await mark(
        "baseline",
        await existingArtifactNames(artifactRoot, captureArtifactNames(fixture, "baseline")),
      );
      await server.stop();
      server = undefined;

      await clearGeneratedCaches(driverRoot);
      const sourcePath = await checkedMigrationPath(
        driverRoot,
        await realpath(driverRoot),
        fixture.source.path,
      );
      assert.ok(
        (await readFile(sourcePath, "utf8")).includes(fixture.source.before),
        "production smoke source token before migration",
      );
      const treeBeforeFirst = await snapshotMigrationSources(driverRoot);
      const cli = await installedEntrypoint(installed.root, "bin");
      await mark("first-cli-started");
      await run(process.execPath, [cli, "--write"], {
        cwd: driverRoot,
        logPath: join(artifactRoot, "first-cli.log"),
      });
      const treeAfterFirst = await snapshotMigrationSources(driverRoot);
      assert.notDeepEqual(
        treeAfterFirst,
        treeBeforeFirst,
        "first CLI migration changes source-scoped files",
      );
      assert.ok(
        !(await readFile(sourcePath, "utf8")).includes(fixture.source.before),
        "production smoke rewrites the target stylesheet",
      );
      await mark("first-cli", ["first-cli.log"]);

      await mark("second-cli-started");
      await run(process.execPath, [cli, "--write"], {
        cwd: driverRoot,
        logPath: join(artifactRoot, "second-cli.log"),
      });
      assert.deepEqual(
        await snapshotMigrationSources(driverRoot),
        treeAfterFirst,
        "second CLI run leaves source-scoped files unchanged",
      );
      await mark("second-cli", ["second-cli.log"]);

      await clearGeneratedCaches(driverRoot);
      await mark("post-build-started");
      await run(npm, ["run", "build"], {
        cwd: driverRoot,
        logPath: join(artifactRoot, "post-build.log"),
      });
      await mark("post-build", ["post-build.log"]);
      server = await startServer(fixture, driverRoot, artifactRoot, "post", "preview");
      const post = await capture("post", server.url);
      await mark(
        "post",
        await existingArtifactNames(artifactRoot, captureArtifactNames(fixture, "post")),
      );
      assert.deepEqual(
        Object.fromEntries(Object.entries(post).map(([name, value]) => [name, value.elements])),
        Object.fromEntries(Object.entries(baseline).map(([name, value]) => [name, value.elements])),
        "production pre/post computed styles, identity, count, and order",
      );
      await mark("complete");
      return { baseline, post, ledger };
    },
  );
}

export async function runExternalLifecycle({
  browser,
  project,
  packageFixture,
  artifactRoot,
  packageArtifactRoot = process.env.ECOSYSTEM_PACKAGE_ARTIFACT_ROOT,
}: {
  browser: Browser;
  project: ExternalProject;
  packageFixture: ControlledProject;
  artifactRoot?: string;
  packageArtifactRoot?: string;
}): Promise<LifecycleResult> {
  if (process.env.CI !== "true" || process.env.ECOSYSTEM_EXTERNAL !== "1") {
    throw new Error("external ecosystem cases require CI=true and ECOSYSTEM_EXTERNAL=1");
  }
  let temporaryRoot: string | undefined;
  if (!artifactRoot) {
    temporaryRoot = await temporaryDirectory(`tw-migrate-${project.id}-artifacts-`);
    artifactRoot = join(temporaryRoot, "artifacts", project.id);
  }
  artifactRoot = resolve(artifactRoot);
  packageArtifactRoot = resolve(packageArtifactRoot as string);
  await rm(artifactRoot, { recursive: true, force: true });
  await mkdir(artifactRoot, { recursive: true });
  let server: RunningServer | undefined;
  return executeLifecycle(
    { project, artifactRoot, temporaryRoot, activeServer: () => server },
    async ({ ledger, mark, diagnostic, runRoot }) => {
      const capture = capturePhase(browser, project.probes, artifactRoot as string, diagnostic);
      const { installed } = await prepareDriver(
        packageFixture,
        runRoot,
        packageArtifactRoot as string,
        artifactRoot as string,
      );
      const { migrate } = await import(
        `${pathToFileURL(await installedEntrypoint(installed.root, "main")).href}?case=${Date.now()}`
      );
      await mark("package-installed", [
        "install.log",
        "publish.log",
        "registry-bootstrap.log",
        "registry-install.log",
      ]);
      const checkoutRoot = await checkoutExternalProject(project, runRoot, artifactRoot);
      await checkedMigrationPath(checkoutRoot, await realpath(checkoutRoot), project.lockfile);
      const runtimeWriteOriginals = await snapshotRuntimeWrites(
        checkoutRoot,
        project.runtimeWrites,
      );
      await mark("checked-out", ["checkout.log"]);

      for (const [index, install] of project.installs.entries()) {
        const cwd = await checkedProjectDirectory(checkoutRoot, install.cwd);
        const invocation = packageManagerInvocation(project, install.args);
        await run(invocation.command, invocation.args, {
          cwd,
          env: externalEnvironment(),
          logPath: join(artifactRoot, `external-install-${index + 1}.log`),
          timeoutMs: 600_000,
        });
      }
      assertTrackedCheckoutClean(checkoutRoot, "installation");
      await mark(
        "external-installed",
        project.installs.map((_, index) => `external-install-${index + 1}.log`),
      );

      const packageRoot = await checkedProjectDirectory(checkoutRoot, project.packageRoot);
      const canonicalPackageRoot = await realpath(packageRoot);
      const sourcePath = await checkedMigrationPath(
        packageRoot,
        canonicalPackageRoot,
        project.source.path,
      );
      await checkedMigrationPath(packageRoot, canonicalPackageRoot, project.tailwindCss);
      const authored = await readFile(sourcePath, "utf8");
      assert.ok(authored.includes(project.source.before), "external source token before migration");

      await mark("baseline-started");
      server = await startExternalServer(project, packageRoot, artifactRoot, "baseline");
      const baseline = await capture("baseline", server.url);
      await mark(
        "baseline",
        await existingArtifactNames(artifactRoot, captureArtifactNames(project, "baseline")),
      );
      await server.stop();
      server = undefined;
      await restoreRuntimeWrites(checkoutRoot, runtimeWriteOriginals, "baseline");

      await writeFile(
        await checkedMigrationPath(packageRoot, canonicalPackageRoot, project.source.path),
        "",
      );
      await mark("causal-witness-started");
      server = await startExternalServer(project, packageRoot, artifactRoot, "withheld");
      const withheld = await capture("withheld", server.url);
      await server.stop();
      server = undefined;
      await writeFile(
        await checkedMigrationPath(packageRoot, canonicalPackageRoot, project.source.path),
        authored,
      );
      await restoreRuntimeWrites(checkoutRoot, runtimeWriteOriginals, "causal witness");
      await mark(
        "causal-witness",
        await existingArtifactNames(artifactRoot, captureArtifactNames(project, "withheld")),
      );

      await mark("migration-started");
      const first = await migrate({
        cwd: packageRoot,
        styleFile: project.source.path,
        tailwindCss: project.tailwindCss,
        write: true,
      });
      await writeFile(
        join(artifactRoot, "first-report.json"),
        `${JSON.stringify(first, null, 2)}\n`,
      );
      await writeFile(join(artifactRoot, "source.diff"), first.diff);
      assert.ok(first.changedFiles.length > 0, "external first migration changes source");
      assert.ok(
        first.candidates.includes(project.source.after),
        "external migration emits expected witness candidate",
      );
      const migratedSource = await readMaybe(sourcePath);
      assert.ok(
        !migratedSource?.includes(project.source.before),
        "external migration rewrites target source",
      );
      const treeBeforeSecond = await snapshotMigrationSources(packageRoot);
      const second = await migrate(
        migratedSource === null
          ? { cwd: packageRoot, tailwindCss: project.tailwindCss, write: true }
          : {
              cwd: packageRoot,
              styleFile: project.source.path,
              tailwindCss: project.tailwindCss,
              write: true,
            },
      );
      await writeFile(
        join(artifactRoot, "second-report.json"),
        `${JSON.stringify(second, null, 2)}\n`,
      );
      assert.deepEqual(second.changedFiles, [], "external second migration changedFiles");
      assert.equal(second.diff, "", "external second migration diff");
      assert.deepEqual(
        await snapshotMigrationSources(packageRoot),
        treeBeforeSecond,
        "external source tree after second migration",
      );
      await mark("migration", ["first-report.json", "second-report.json", "source.diff"]);

      if (migratedSource !== null) {
        await writeFile(
          await checkedMigrationPath(packageRoot, canonicalPackageRoot, project.source.path),
          "",
        );
      }
      let utilitiesExpectedDiff: Buffer | undefined;
      let unsafeSourceError: Error | undefined;
      await mark("utilities-only-started");
      try {
        await clearGeneratedCaches(packageRoot);
        utilitiesExpectedDiff = trackedCheckoutDiff(checkoutRoot);
        server = await startExternalServer(project, packageRoot, artifactRoot, "utilities-only");
        const utilitiesOnly = await capture("utilities-only", server.url);
        assert.deepEqual(
          Object.fromEntries(
            Object.entries(utilitiesOnly).map(([name, value]) => [name, value.elements]),
          ),
          Object.fromEntries(
            Object.entries(baseline).map(([name, value]) => [name, value.elements]),
          ),
          "external utilities-only computed capture exactly equals baseline",
        );
        await mark(
          "utilities-only",
          await existingArtifactNames(
            artifactRoot,
            captureArtifactNames(project, "utilities-only"),
          ),
        );
      } finally {
        await server?.stop();
        server = undefined;
        if (utilitiesExpectedDiff) {
          await restoreRuntimeWrites(
            checkoutRoot,
            runtimeWriteOriginals,
            "utilities-only",
            utilitiesExpectedDiff,
          );
        }
        if (migratedSource === null) {
          const recreated = await lstat(sourcePath).catch(() => null);
          if (recreated?.isSymbolicLink() || (recreated && !recreated.isFile())) {
            unsafeSourceError = new Error(
              `external server recreated an unsafe migration path: ${project.source.path}`,
            );
          } else {
            await rm(sourcePath, { force: true });
          }
        } else {
          await writeFile(
            await checkedMigrationPath(packageRoot, canonicalPackageRoot, project.source.path),
            migratedSource,
          );
        }
      }

      if (unsafeSourceError) throw unsafeSourceError;

      await clearGeneratedCaches(packageRoot);
      const postExpectedDiff = trackedCheckoutDiff(checkoutRoot);
      await mark("post-started");
      server = await startExternalServer(project, packageRoot, artifactRoot, "post");
      const post = await capture("post", server.url);
      await server.stop();
      server = undefined;
      await restoreRuntimeWrites(checkoutRoot, runtimeWriteOriginals, "post", postExpectedDiff);
      await mark(
        "post",
        await existingArtifactNames(artifactRoot, captureArtifactNames(project, "post")),
      );
      assertOracle({ baseline, post, withheld, candidateTokens: first.candidates });
      await mark("complete");
      return { first, second, baseline, post, ledger };
    },
  );
}
