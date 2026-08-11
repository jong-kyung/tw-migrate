import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdtemp, mkdir, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "vite-plus/test";

import {
  assertInstalledLayout,
  currentTarget,
  packageUploadRoot,
  preparePackageUpload,
  publisherToken,
  stageRootPackage,
  validateProvenance,
} from "../ecosystem-ci/packages.ts";
import { registryConfig } from "../ecosystem-ci/registry.ts";
import {
  assertOracle,
  captureProbe,
  normalizeStyleEntries,
  retryCapture,
  withTimeout,
} from "../ecosystem-ci/oracle.ts";
import {
  artifactAllowlist,
  assertExpectedChangedFiles,
  assertMigrationContract,
  captureAttemptArtifactNames,
  externalEnvironment,
  prepareCaseUpload,
  restoreRuntimeWrites,
  runExternalLifecycle,
  snapshotMigrationSources,
  teardownLifecycleServer,
} from "../ecosystem-ci/lifecycle.ts";
import { waitForChild } from "../ecosystem-ci/shared.ts";
import { loadManifest, runHarness, validateManifest, vitestProjects } from "../ecosystem-ci/run.ts";
import type { AddressInfo } from "node:net";
import type { Browser } from "playwright";

import type {
  CaptureSet,
  ControlledProject,
  ExternalProject,
  MigrationReport,
  Probe,
  ProbeCapture,
  Project,
  Provenance,
} from "../ecosystem-ci/types.ts";

// Manifest fixtures are deliberately mutated into invalid shapes to exercise
// the validator, so the builders hand back indexable records.
type Fixture = Record<string, any>;
type ExecFailure = { status: number; stderr: string };

const isControlled = (project: Project): project is ControlledProject =>
  project.kind === "controlled";
const isExternal = (project: Project): project is ExternalProject => project.kind === "external";

const selector = { type: "role", value: "button", name: "Toggle details" };
const desktop = { width: 1280, height: 720 };
const mobile = { width: 375, height: 667 };

function probe(overrides: Fixture = {}): Fixture {
  return {
    route: "/",
    viewport: desktop,
    readiness: { selector, cardinality: 1 },
    selector: { type: "data", value: "card" },
    cardinality: 1,
    identity: ["card"],
    ...overrides,
  };
}

function controlled(overrides: Fixture = {}): Fixture {
  return {
    id: "react-vite-css",
    kind: "controlled",
    runtime: "react-vite",
    style: "css",
    source: { path: "src/App.module.css", before: ".card", after: "p-[13px]" },
    probes: {
      base: probe(),
      hover: probe({ action: { type: "hover", selector } }),
      focus: probe({ action: { type: "focus", selector } }),
      "focus-visible": probe({ action: { type: "press", key: "Tab" } }),
      "responsive-below": probe({ viewport: mobile }),
      "responsive-above": probe(),
    },
    ...overrides,
  };
}

function external(overrides: Fixture = {}): Fixture {
  const base = controlled();
  return {
    id: "external",
    kind: "external",
    repository: "https://example.test/project.git",
    revision: "0123456789abcdef0123456789abcdef01234567",
    packageManager: "pnpm@10.0.0",
    lockfile: "pnpm-lock.yaml",
    packageRoot: ".",
    installs: [{ cwd: ".", args: ["install", "--frozen-lockfile", "--ignore-scripts"] }],
    runtimeWrites: [],
    start: ["run", "dev"],
    tailwindCss: "src/tailwind.css",
    source: base.source,
    probes: { base: base.probes.base },
    ...overrides,
  };
}

function manifest(...projects: unknown[]) {
  return { projects };
}

async function tempRoot(t: {
  onTestFinished: (handler: () => Promise<void>) => void;
}): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "tw-migrate-test-"));
  t.onTestFinished(() => rm(root, { recursive: true, force: true }));
  return root;
}

function errorFor(projects: unknown[]) {
  assert.throws(() => validateManifest(manifest(...projects)));
}

async function readEcosystemWorkflow() {
  return (
    await readFile(new URL("../.github/workflows/ecosystem.yml", import.meta.url), "utf8")
  ).replaceAll("\r\n", "\n");
}

test("admits the complete controlled runtime and stylesheet matrix", async () => {
  const loaded = await loadManifest();
  assert.deepEqual(
    loaded.projects
      .filter(isControlled)
      .map(({ id, kind, runtime, style }) => [id, kind, runtime, style]),
    [
      ["react-vite-css", "controlled", "react-vite", "css"],
      ["react-vite-scss", "controlled", "react-vite", "scss"],
      ["react-vite-sass", "controlled", "react-vite", "sass"],
      ["react-vite-less", "controlled", "react-vite", "less"],
      ["next-css", "controlled", "next", "css"],
      ["next-scss", "controlled", "next", "scss"],
      ["next-sass", "controlled", "next", "sass"],
      ["next-less", "controlled", "next", "less"],
      ["vite-html-css", "controlled", "vite-html", "css"],
      ["vite-html-scss", "controlled", "vite-html", "scss"],
      ["vite-html-sass", "controlled", "vite-html", "sass"],
      ["vite-html-less", "controlled", "vite-html", "less"],
      ["vue-vite-css", "controlled", "vue-vite", "css"],
      ["media-components", "controlled", "react-vite", "css"],
      ["media-stacked", "controlled", "react-vite", "css"],
      ["media-workspace", "controlled", "react-vite", "css"],
      ["media-workspace-split", "controlled", "react-vite", "css"],
    ],
  );
  assert.deepEqual(
    loaded.projects.filter(({ kind }) => kind === "smoke"),
    [{ id: "production-react-vite-css", kind: "smoke", fixture: "react-vite-css" }],
  );
  assert.deepEqual(
    loaded.projects.filter(isExternal).map(({ id, revision }) => [id, revision]),
    [
      ["external-namechecker", "285e10d3627f3eac5217d69e9eaccee956d7ac70"],
      ["external-stylized-components", "a26df5d21457095e466a41966822edb2ff016cff"],
    ],
  );
});

test("smoke and external cases accept non-exhaustive probes without occupying controlled matrix cells", () => {
  const base = controlled();
  const probeFields = {
    source: base.source,
    probes: {
      base: probe(),
      details: probe({ action: { type: "hover", selector } }),
    },
  };
  assert.doesNotThrow(() =>
    validateManifest(
      manifest(base, { id: "smoke", kind: "smoke", fixture: base.id }, external(probeFields)),
    ),
  );
  errorFor([base, { id: "smoke", kind: "smoke", fixture: "missing" }]);
});

test("rejects invalid manifests before execution", () => {
  errorFor([controlled(), controlled({ style: "scss" })]);
  errorFor([controlled({ extra: true })]);
  errorFor([controlled(), controlled({ id: "same-cell" })]);
  errorFor([external({ revision: "abc123" })]);
  const missingSource = controlled();
  delete missingSource.source;
  errorFor([missingSource]);
  errorFor([controlled({ probes: {} })]);
  errorFor([
    controlled({
      fixture: "react-vite/css",
      probes: { base: { ...probe(), witness: false } },
    }),
  ]);
  errorFor([external({ probes: { base: { ...probe(), witness: false } } })]);
  errorFor([external({ probes: {} })]);
  errorFor([external({ probes: { base: probe({ action: { type: "hovre", selector } }) } })]);
  errorFor([external({ probes: { base: probe({ selector: { type: "datta", value: "card" } }) } })]);
});

test("migration source paths must stay relative and inside the driver", () => {
  for (const path of [
    "../outside.css",
    "src/../../outside.css",
    "/outside.css",
    "C:\\outside.css",
  ]) {
    errorFor([controlled({ source: { ...controlled().source, path } })]);
  }
});

test("external commands must be argument arrays rather than shell strings", () => {
  errorFor([external({ installs: [{ cwd: ".", args: "install --frozen-lockfile" }] })]);
  errorFor([external({ start: "run dev" })]);
});

test("external commands receive only the explicit non-secret environment", () => {
  process.env.ECOSYSTEM_SENTINEL_SECRET = "must-not-leak";
  try {
    const env = externalEnvironment();
    assert.equal(env.ECOSYSTEM_SENTINEL_SECRET, undefined);
    assert.equal(env.CI, "true");
    assert.equal(env.PATH, process.env.PATH);
  } finally {
    delete process.env.ECOSYSTEM_SENTINEL_SECRET;
  }
});

test("post-migration server phases preserve the expected tracked diff", async (t) => {
  const root = await tempRoot(t);
  const git = (args: string[]) => execFileSync("git", args, { cwd: root, stdio: "pipe" });
  git(["init", "-q"]);
  git(["config", "user.email", "test@example.com"]);
  git(["config", "user.name", "Test"]);
  await Promise.all([
    writeFile(join(root, "src.css"), "original\n"),
    writeFile(join(root, "tsconfig.json"), "original\n"),
  ]);
  git(["add", "."]);
  git(["commit", "-qm", "fixture"]);

  await writeFile(join(root, "src.css"), "migrated\n");
  const expectedDiff = git(["diff", "HEAD", "--binary", "--no-ext-diff", "--no-textconv", "--"]);
  const originals = { "tsconfig.json": Buffer.from("original\n") };
  await writeFile(join(root, "tsconfig.json"), "framework update\n");
  await assert.doesNotReject(restoreRuntimeWrites(root, originals, "post", expectedDiff));
  assert.equal(await readFile(join(root, "tsconfig.json"), "utf8"), "original\n");

  await writeFile(join(root, "src.css"), "server regression\n");
  await assert.rejects(
    restoreRuntimeWrites(root, originals, "post", expectedDiff),
    /unreviewed tracked files/,
  );
});

test("external repositories and paths stay inside the CI checkout trust boundary", () => {
  for (const repository of [
    "http://example.test/project.git",
    "ssh://git@example.test/project.git",
    "file:///tmp/project",
    "https://user@example.test/project.git",
    "https://example.test/project.git?ref=main",
  ])
    errorFor([external({ repository })]);
  for (const field of ["packageRoot", "tailwindCss"])
    errorFor([external({ [field]: "../outside" })]);
  errorFor([
    external({
      installs: [{ cwd: "../outside", args: ["install", "--frozen-lockfile", "--ignore-scripts"] }],
    }),
  ]);
  errorFor([
    external({
      installs: Array.from({ length: 5 }, () => ({
        cwd: ".",
        args: ["install", "--frozen-lockfile", "--ignore-scripts"],
      })),
    }),
  ]);
  for (const args of [
    ["exec", "sh"],
    ["run", "dev"],
    ["install", "--frozen-lockfile"],
  ]) {
    errorFor([external({ installs: [{ cwd: ".", args }] })]);
  }
  for (const start of [
    ["exec", "sh"],
    ["run", "dev", "--", "--host"],
  ])
    errorFor([external({ start })]);
  errorFor([external({ runtimeWrites: ["src/App.module.css"] })]);
  errorFor([external({ runtimeWrites: ["a", "b", "c", "d"] })]);
});

test("stages concrete optional dependency versions without changing the tracked manifest", async (t) => {
  const root = await tempRoot(t);
  const repoRoot = join(root, "repo");
  const stageRoot = join(root, "stage");
  await mkdir(repoRoot);
  const tracked = `${JSON.stringify(
    {
      name: "tw-migrate",
      version: "1.2.3",
      files: ["index.js"],
      optionalDependencies: {
        "tw-migrate-darwin-arm64": "workspace:1.2.3",
        "tw-migrate-linux-x64-gnu": "workspace:1.2.3",
      },
    },
    null,
    2,
  )}\n`;
  await Promise.all([
    writeFile(join(repoRoot, "package.json"), tracked),
    writeFile(join(repoRoot, "index.js"), "export const migrate = () => {};\n"),
    writeFile(join(repoRoot, "README.md"), "readme\n"),
    writeFile(join(repoRoot, "LICENSE"), "license\n"),
  ]);

  await stageRootPackage({ repoRoot, stageRoot });

  assert.equal(await readFile(join(repoRoot, "package.json"), "utf8"), tracked);
  const staged = JSON.parse(await readFile(join(stageRoot, "package.json"), "utf8"));
  assert.deepEqual(Object.values(staged.optionalDependencies), ["1.2.3", "1.2.3"]);
});

test("package stage CLI creates the exact upload tree consumed by the workflow", async (t) => {
  const root = await tempRoot(t);
  const artifactRoot = join(root, "package-artifacts");
  const target = currentTarget();
  const provenance = {
    packages: {
      root: { tarball: "tarballs/root.tgz" },
      native: { tarball: "tarballs/native.tgz" },
    },
    addon: { file: `staging/native/${target.addon}` },
  } as unknown as Provenance;
  await Promise.all([
    mkdir(join(artifactRoot, "tarballs"), { recursive: true }),
    mkdir(join(artifactRoot, "staging", "native"), { recursive: true }),
  ]);
  await Promise.all([
    writeFile(join(artifactRoot, "provenance.json"), "{}\n"),
    writeFile(join(artifactRoot, provenance.packages.root.tarball), "root"),
    writeFile(join(artifactRoot, provenance.packages.native.tarball), "native"),
    writeFile(join(artifactRoot, provenance.addon.file), "addon"),
  ]);

  const uploadRoot = packageUploadRoot(artifactRoot);
  await preparePackageUpload(provenance, artifactRoot, uploadRoot);

  assert.equal(uploadRoot, `${artifactRoot}-upload`);
  assert.deepEqual((await readdir(uploadRoot)).sort(), ["provenance.json", "staging", "tarballs"]);
  assert.deepEqual((await readdir(join(uploadRoot, "tarballs"))).sort(), [
    "native.tgz",
    "root.tgz",
  ]);
  assert.deepEqual(await readdir(join(uploadRoot, "staging")), ["native"]);
  assert.deepEqual(await readdir(join(uploadRoot, "staging", "native")), [target.addon]);
  const workflow = await readEcosystemWorkflow();
  assert.match(workflow, /packages\.ts stage --artifact-root ecosystem-ci\/package-artifacts/);
  assert.match(workflow, /path: ecosystem-ci\/package-artifacts-upload\//);
});

test("provenance rejects altered tarballs, commits, platforms, and package identities", async (t) => {
  const artifactRoot = await tempRoot(t);
  await Promise.all([
    writeFile(join(artifactRoot, "root.tgz"), "root"),
    writeFile(join(artifactRoot, "native.tgz"), "native"),
    writeFile(join(artifactRoot, currentTarget().addon), "addon"),
  ]);
  const target = currentTarget();
  const provenance = {
    commit: "0123456789abcdef0123456789abcdef01234567",
    platform: target.platform,
    packages: {
      root: {
        name: "tw-migrate",
        version: "1.2.3",
        tarball: "root.tgz",
        sha256: "4813494d137e1631bba301d5acab6e7bb7aa74ce1185d456565ef51d737677b2",
      },
      native: {
        name: target.packageName,
        version: "1.2.3",
        tarball: "native.tgz",
        sha256: "bef32d2c315a289576f2a6828d27edb16bb316a4d85c271f2d794045f3ea668d",
      },
    },
    addon: {
      file: target.addon,
      sha256: "613c3abf0f077f31505d3c8cc0fed9a94a49cf025af3e604c4d38259c1cdf4c7",
    },
  };
  await assert.doesNotReject(
    validateProvenance(provenance, { artifactRoot, expectedCommit: provenance.commit }),
  );

  for (const invalid of [
    { ...provenance, commit: "f".repeat(40) },
    { ...provenance, platform: "wrong-platform" },
    {
      ...provenance,
      packages: {
        ...provenance.packages,
        native: { ...provenance.packages.native, name: "tw-migrate-wrong" },
      },
    },
  ]) {
    await assert.rejects(
      validateProvenance(invalid, { artifactRoot, expectedCommit: provenance.commit }),
    );
  }
  await writeFile(join(artifactRoot, "native.tgz"), "altered");
  await assert.rejects(
    validateProvenance(provenance, { artifactRoot, expectedCommit: provenance.commit }),
    /digest/,
  );
});

test("installed layout rejects checkout, symlink, wrong platform, and unexpected package paths", async (t) => {
  const root = await tempRoot(t);
  const checkout = join(root, "checkout");
  const driverRoot = join(root, "driver");
  const target = currentTarget();
  const rootPackage = join(driverRoot, "node_modules", "tw-migrate");
  const nativePackage = join(driverRoot, "node_modules", target.packageName);
  await Promise.all([
    mkdir(checkout),
    mkdir(rootPackage, { recursive: true }),
    mkdir(nativePackage, { recursive: true }),
  ]);
  await Promise.all([
    writeFile(
      join(rootPackage, "package.json"),
      JSON.stringify({ name: "tw-migrate", version: "1.2.3" }),
    ),
    writeFile(
      join(nativePackage, "package.json"),
      JSON.stringify({ name: target.packageName, version: "1.2.3" }),
    ),
    writeFile(join(nativePackage, target.addon), "addon"),
  ]);
  const expected = {
    version: "1.2.3",
    platform: target.platform,
    addonSha256: "613c3abf0f077f31505d3c8cc0fed9a94a49cf025af3e604c4d38259c1cdf4c7",
  };
  await assert.doesNotReject(
    assertInstalledLayout({ driverRoot, checkoutRoot: checkout, expected }),
  );
  await assert.rejects(
    assertInstalledLayout({ driverRoot, checkoutRoot: root, expected }),
    /checkout/,
  );
  await assert.rejects(
    assertInstalledLayout({
      driverRoot,
      checkoutRoot: checkout,
      expected: { ...expected, platform: "wrong-platform" },
    }),
  );

  await rm(nativePackage, { recursive: true });
  await mkdir(join(checkout, target.packageName));
  await writeFile(
    join(checkout, target.packageName, "package.json"),
    JSON.stringify({ name: target.packageName, version: "1.2.3" }),
  );
  await writeFile(join(checkout, target.packageName, target.addon), "addon");
  await mkdir(join(driverRoot, "node_modules"), { recursive: true });
  await symlink(join(checkout, target.packageName), nativePackage);
  await assert.rejects(
    assertInstalledLayout({ driverRoot, checkoutRoot: checkout, expected }),
    /checkout|node_modules|workspace/,
  );
});

test("publisher credential response body is bounded by the registry startup timeout", async (t) => {
  let requested = false;
  const server = createServer((_request, response) => {
    requested = true;
    response.writeHead(200, { "content-type": "application/json" });
    response.write('{"token":"');
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", () => resolve()));
  t.onTestFinished(() => new Promise<void>((resolve) => server.close(() => resolve())));

  const { port } = server.address() as AddressInfo;
  await assert.rejects(publisherToken(`http://127.0.0.1:${port}`, 250), { name: "TimeoutError" });
  assert.equal(requested, true);
});

test("sealed registry config proxies dependencies but never product packages or mutations", () => {
  const config = registryConfig({ storage: "/tmp/storage", allowPublish: false });
  assert.match(config, /tw-migrate-\*/);
  assert.match(config, /proxy: false/);
  assert.match(config, /publish: nobody/);
  assert.match(config, /'\*\*':[\s\S]*proxy: npmjs/);
});

test("--case selects exactly one project and maps it to a Vitest project filter", async () => {
  const loaded = await loadManifest();
  const calls: string[][] = [];
  const selected = runHarness(["--case", "react-vite-css"], loaded, (args) => calls.push(args));
  assert.deepEqual(
    selected.map(({ id }) => id),
    ["react-vite-css"],
  );
  assert.deepEqual(calls, [
    ["run", "--config", "ecosystem-ci/vitest.config.ts", "--project", "react-vite-css"],
  ]);
});

async function withEnv(
  vars: Record<string, string | undefined>,
  fn: () => Promise<void> | void,
): Promise<void> {
  const previous = Object.fromEntries(Object.keys(vars).map((name) => [name, process.env[name]]));
  for (const [name, value] of Object.entries(vars)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  try {
    await fn();
  } finally {
    for (const [name, value] of Object.entries(previous)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  }
}

test("Vitest and the lifecycle omit external projects unless the CI-only gate is active", async () => {
  const projects = (await loadManifest()).projects;
  assert.equal(
    vitestProjects(projects, {}).some(({ kind }) => kind === "external"),
    false,
  );
  assert.equal(
    vitestProjects(projects, { CI: "true", ECOSYSTEM_EXTERNAL: "1" }).filter(
      ({ kind }) => kind === "external",
    ).length,
    2,
  );
  await withEnv({ CI: undefined, ECOSYSTEM_EXTERNAL: undefined }, () =>
    assert.rejects(
      runExternalLifecycle({} as Parameters<typeof runExternalLifecycle>[0]),
      /require CI=true/,
    ),
  );
});

test("external cases require the explicit CI-only entrypoint", async () => {
  const loaded = await loadManifest();
  assert.throws(
    () =>
      runHarness(["--case", "external-stylized-components"], loaded, () =>
        assert.fail("must not execute"),
      ),
    /CI-only/,
  );
  assert.throws(
    () =>
      runHarness(["--external-case", "external-stylized-components"], loaded, () =>
        assert.fail("must not execute"),
      ),
    /CI-only/,
  );
  await withEnv({ CI: "true", ECOSYSTEM_EXTERNAL: "1" }, () => {
    const calls: string[][] = [];
    const selected = runHarness(
      ["--external-case", "external-stylized-components"],
      loaded,
      (args) => calls.push(args),
    );
    assert.deepEqual(
      selected.map(({ id }) => id),
      ["external-stylized-components"],
    );
    assert.deepEqual(calls, [
      [
        "run",
        "--config",
        "ecosystem-ci/vitest.config.ts",
        "--project",
        "external-stylized-components",
      ],
    ]);
  });
});

test("unknown case prints the available ids without executing Vitest", async () => {
  const loaded = await loadManifest();
  const message = /Unknown case "missing".*react-vite-css.*next-css.*vite-html-css/;
  assert.throws(
    () => runHarness(["--case", "missing"], loaded, () => assert.fail("must not execute")),
    message,
  );
  assert.throws(
    () =>
      execFileSync(process.execPath, ["ecosystem-ci/run.ts", "--case", "missing"], {
        encoding: "utf8",
        stdio: "pipe",
      }),
    (error: unknown) => {
      const failure = error as ExecFailure;
      return failure.status === 1 && message.test(failure.stderr);
    },
  );
});

test("no arguments print usage and --all is the only full-run selection", async () => {
  const loaded = await loadManifest();
  assert.throws(() => runHarness([], loaded, () => assert.fail("must not execute")), /Usage:/);
  const calls: string[][] = [];
  const selected = runHarness(["--all"], loaded, (args) => calls.push(args));
  assert.equal(selected.length, 17);
  assert.deepEqual(calls, [
    [
      "run",
      "--config",
      "ecosystem-ci/vitest.config.ts",
      ...selected.flatMap(({ id }) => ["--project", id]),
    ],
  ]);
});

test("the browser oracle sorts every standard computed property and excludes only custom properties", () => {
  assert.deepEqual(
    normalizeStyleEntries([
      ["z-index", "auto"],
      ["--fixture-token", "secret"],
      ["-webkit-font-smoothing", "auto"],
      ["color", "rgb(1, 2, 3)"],
    ]),
    {
      "-webkit-font-smoothing": "auto",
      color: "rgb(1, 2, 3)",
      "z-index": "auto",
    },
  );
});

test("the causal witness requires a standard computed-property change for every probe", () => {
  const capture = (color: string, token: string) =>
    ({
      elements: [{ identity: "card", styles: { color, "--fixture-token": token } }],
    }) as unknown as ProbeCapture;
  const baseline: CaptureSet = { base: capture("red", "one"), hover: capture("red", "one") };
  assert.throws(
    () =>
      assertOracle({
        baseline,
        post: structuredClone(baseline),
        withheld: { base: capture("black", "two"), hover: capture("red", "two") },
        candidateTokens: ["text-red"],
      }),
    /standard computed property for probe "hover"/,
  );
});

test("capture retry permits initial plus three retries and creates fresh attempts", async () => {
  const attempts: number[] = [];
  const result = await retryCapture(async (attempt) => {
    attempts.push(attempt);
    if (attempt < 4) throw new Error("navigation failed");
    return "captured";
  });
  assert.equal(result, "captured");
  assert.deepEqual(attempts, [1, 2, 3, 4]);
  assert.deepEqual(captureAttemptArtifactNames("baseline", "base", 3), [
    "baseline-base-attempt-3-browser.json",
    "baseline-base-attempt-3.png",
  ]);
  await assert.rejects(
    retryCapture(async () => {
      throw new Error("still broken");
    }),
    /still broken/,
  );
});

test("capture attempt timeout rejects a stalled operation", async () => {
  await assert.rejects(
    withTimeout(() => new Promise(() => {}), 10),
    /timed out after 10ms/,
  );
});

test("page-creation failures keep diagnostics for all four attempts", async () => {
  const diagnostics: { attempt: number; error: string }[] = [];
  await assert.rejects(
    captureProbe(
      {
        newPage: async () => {
          throw new Error("browser unavailable");
        },
      } as unknown as Browser,
      "http://127.0.0.1/",
      probe() as Probe,
      (attempt) => ({
        screenshot: "",
        writeDiagnostics: async (value: unknown) => {
          diagnostics.push({ attempt, ...(value as { error: string }) });
        },
      }),
    ),
    /browser unavailable/,
  );
  assert.deepEqual(
    diagnostics.map(({ attempt }) => attempt),
    [1, 2, 3, 4],
  );
  assert.ok(diagnostics.every(({ error }) => error.includes("browser unavailable")));
});

test("migration contract checks the exact report/source and no-op second run without retries", () => {
  const first = {
    changedFiles: ["src/a.css"],
    diff: "diff",
    candidates: ["p-[13px]"],
    warnings: [],
  } as unknown as MigrationReport;
  assert.doesNotThrow(() =>
    assertMigrationContract({
      first,
      expectedFirst: first,
      actualSource: "after\n",
      expectedSource: "after\n",
      second: { changedFiles: [], diff: "" } as unknown as MigrationReport,
      treeBeforeSecond: { "src/a.css": "abc" },
      treeAfterSecond: { "src/a.css": "abc" },
    }),
  );
  assert.throws(
    () =>
      assertMigrationContract({
        first,
        expectedFirst: first,
        actualSource: "wrong",
        expectedSource: "after\n",
        second: { changedFiles: [], diff: "" } as unknown as MigrationReport,
        treeBeforeSecond: {},
        treeAfterSecond: {},
      }),
    /source/,
  );
});

test("source-wide idempotency catches an unreported extra source mutation", async (t) => {
  const root = await tempRoot(t);
  await mkdir(join(root, "src"));
  await Promise.all([
    writeFile(join(root, "src", "reported.css"), "reported\n"),
    writeFile(join(root, "src", "unreported.tsx"), "before\n"),
  ]);
  const before = await snapshotMigrationSources(root);
  await writeFile(join(root, "src", "unreported.tsx"), "after\n");
  const after = await snapshotMigrationSources(root);
  assert.throws(
    () =>
      assertMigrationContract({
        first: { changedFiles: ["src/reported.css"] } as unknown as MigrationReport,
        expectedFirst: { changedFiles: ["src/reported.css"] } as unknown as MigrationReport,
        actualSource: "reported\n",
        expectedSource: "reported\n",
        second: { changedFiles: [], diff: "" } as unknown as MigrationReport,
        treeBeforeSecond: before,
        treeAfterSecond: after,
      }),
    /source-scoped tree/,
  );
});

test("source snapshots exclude generated trees and reject non-regular paths", async (t) => {
  const root = await tempRoot(t);
  await mkdir(join(root, "node_modules"));
  await writeFile(join(root, "node_modules", "generated.js"), "ignored\n");
  assert.deepEqual(await snapshotMigrationSources(root), {});
  await symlink(join(root, "node_modules", "generated.js"), join(root, "linked.js"));
  await assert.rejects(snapshotMigrationSources(root), /not regular/);
});

test("exact changed-file validation requires complete paths and bytes", () => {
  const changedFiles = ["src/App.jsx", "src/App.css"];
  const files = { "src/App.jsx": "consumer\n", "src/App.css": "style\n" };
  assert.doesNotThrow(() => assertExpectedChangedFiles(changedFiles, files, files));
  assert.throws(
    () => assertExpectedChangedFiles(changedFiles, { "src/App.css": "style\n" }, files),
    /cover changedFiles/,
  );
  assert.throws(
    () => assertExpectedChangedFiles(changedFiles, files, { ...files, "src/App.jsx": "wrong\n" }),
    /exact post-migration bytes/,
  );
});

test("controlled expectations cover every reported changed file with exact bytes", async () => {
  for (const runtime of ["react-vite", "next", "vite-html"]) {
    for (const style of ["css", "scss", "sass", "less"]) {
      const expected = JSON.parse(
        await readFile(
          new URL(
            `../ecosystem-ci/fixtures/controlled/${runtime}/${style}/expected.json`,
            import.meta.url,
          ),
          "utf8",
        ),
      );
      assert.deepEqual(
        Object.keys(expected.changedFiles).sort(),
        [...expected.first.changedFiles].sort((left, right) => left.localeCompare(right)),
      );
      assert.ok(
        Object.values(expected.changedFiles).every((contents) => typeof contents === "string"),
      );
    }
  }
});

test("command timeout terminates the child before rejecting", async () => {
  const child = spawn(process.execPath, ["-e", "setTimeout(() => {}, 60_000)"], {
    detached: process.platform !== "win32",
    stdio: "ignore",
    windowsHide: true,
  });
  await assert.rejects(waitForChild(child, { timeoutMs: 200 }), /timed out after 200ms/);
  assert.notEqual(child.exitCode ?? child.signalCode, null);
});

test("final server teardown records and propagates only when lifecycle otherwise succeeded", async () => {
  const failure = new Error("stop failed");
  const recorded: unknown[] = [];
  const server = {
    url: "",
    stop: async () => {
      throw failure;
    },
  };
  await assert.rejects(
    teardownLifecycleServer(server, undefined, async (error) => {
      recorded.push(error);
    }),
    failure,
  );
  assert.deepEqual(recorded, [failure]);
  await assert.doesNotReject(
    teardownLifecycleServer(server, new Error("primary"), async () =>
      assert.fail("must preserve primary"),
    ),
  );
});

test("workflow artifact allowlist rejects traversal, symlinks, directories, and undeclared files", async (t) => {
  const root = await tempRoot(t);
  await writeFile(join(root, "phase-ledger.json"), "{}");
  assert.deepEqual(await artifactAllowlist(root, ["phase-ledger.json"]), [
    join(root, "phase-ledger.json"),
  ]);
  await assert.rejects(artifactAllowlist(root, ["../outside"]), /escapes/);
  await mkdir(join(root, "directory"));
  await assert.rejects(artifactAllowlist(root, ["directory"]), /regular file/);
  await symlink(join(root, "phase-ledger.json"), join(root, "link"));
  await assert.rejects(artifactAllowlist(root, ["link"]), /regular file|symlink/);
});

test("case failure uploads preserve package publication logs", async (t) => {
  const root = await tempRoot(t);
  const uploadRoot = packageUploadRoot(root);
  await Promise.all([
    writeFile(
      join(root, "phase-ledger.json"),
      `${JSON.stringify({ case: "react-vite-css", phases: [], failure: "publish failed", failureFiles: ["publish.log"] })}\n`,
    ),
    writeFile(join(root, "publish.log"), "npm publish failed\n"),
  ]);

  await prepareCaseUpload(controlled() as ControlledProject, root, uploadRoot);
  assert.equal(await readFile(join(uploadRoot, "publish.log"), "utf8"), "npm publish failed\n");
});

test("case jobs run after non-cancelled partial package failure while preserving label gating", async () => {
  const workflow = await readEcosystemWorkflow();
  assert.match(
    workflow,
    /^  case:\n    needs: package\n    if: \$\{\{ !cancelled\(\) && \(github\.event_name != 'pull_request' \|\| github\.event\.label\.name == 'test:e2e'\) \}\}$/m,
  );
  // Smoke and external cases share the single gated case job.
  assert.match(
    workflow,
    /case: \[.*vue-vite-css, media-components, media-stacked, media-workspace, media-workspace-split, production-react-vite-css, external-namechecker, external-stylized-components\]/,
  );
});

test("the no-argument CLI stays browser-free and returns usage", () => {
  assert.throws(
    () =>
      execFileSync(process.execPath, ["ecosystem-ci/run.ts"], { encoding: "utf8", stdio: "pipe" }),
    (error: unknown) => {
      const failure = error as ExecFailure;
      return failure.status === 1 && /Usage:/.test(failure.stderr);
    },
  );
});
