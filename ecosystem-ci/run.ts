#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, posix, resolve, win32 } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import * as z from "zod/mini";

import { prepareCaseUpload } from "./lifecycle.ts";
import { packageUploadRoot, stagePackages } from "./packages.ts";
import { platformCommand } from "./shared.ts";
import {
  controlledRuntimes,
  controlledStyles,
  selectorTypes as knownSelectorTypes,
} from "./types.ts";
import type { ControlledProject, Manifest, ProbedProject, Project } from "./types.ts";

const usage = "Usage: node ecosystem-ci/run.ts (--case <id> | --all)";
const nonemptyString = z.string().check(z.minLength(1));
// Zod records skip __proto__ before validating values. Reject it on the raw
// input so unknown project keys or authored probes cannot silently disappear.
const recordInput = z
  .unknown()
  .check(
    z.refine(
      (value) => value === null || typeof value !== "object" || !Object.hasOwn(value, "__proto__"),
      "Object keys must not include __proto__",
    ),
  );
// Preserve the existing integer contract without imposing a new safe-integer ceiling.
const positiveInteger = z
  .number()
  .check(z.refine(Number.isInteger, "must be an integer"), z.minimum(1));
const relativePath = nonemptyString.check(
  z.refine(
    (path) =>
      !posix.isAbsolute(path) && !win32.isAbsolute(path) && !path.split(/[\\/]/).includes(".."),
    "must be a relative path without traversal",
  ),
);
const selectorSchema = z
  .strictObject({
    type: z.enum(knownSelectorTypes),
    value: nonemptyString,
    name: z.optional(nonemptyString),
  })
  .check(
    z.refine(
      (selector) => selector.type !== "tag" || /^[a-z][a-z0-9-]*$/.test(selector.value),
      "tag selector must be one lowercase HTML tag",
    ),
    z.refine(
      (selector) => selector.type !== "css" || !/[.#]/.test(selector.value),
      "CSS selector must not contain class or id selectors",
    ),
    z.refine(
      (selector) => selector.name === undefined || selector.type === "role",
      "name is only valid for role selectors",
    ),
  );
const actionSchema = z.discriminatedUnion("type", [
  z.strictObject({ type: z.literal("press"), key: nonemptyString }),
  z.strictObject({ type: z.enum(["hover", "focus"]), selector: selectorSchema }),
]);
const probeSchema = z
  .strictObject({
    route: nonemptyString,
    viewport: z.strictObject({ width: positiveInteger, height: positiveInteger }),
    readiness: z.strictObject({ selector: selectorSchema, cardinality: positiveInteger }),
    selector: selectorSchema,
    cardinality: positiveInteger,
    identity: z.array(nonemptyString),
    action: z.optional(actionSchema),
    witness: z.optional(z.literal(false)),
  })
  .check(
    z.refine(
      (probe) => probe.identity.length === probe.cardinality,
      "identity must contain one stable identity per target",
    ),
  );
const probesSchema = z
  .pipe(recordInput, z.record(z.string(), probeSchema))
  .check(z.refine((probes) => Object.keys(probes).length > 0, "must contain at least one probe"));
const controlledProbesSchema = z
  .strictObject({
    base: probeSchema,
    hover: probeSchema,
    focus: probeSchema,
    "focus-visible": probeSchema,
    "responsive-below": probeSchema,
    "responsive-above": probeSchema,
    // Only fixtures exercising <style module> need this additional observation.
    "module-panel": z.optional(probeSchema),
  })
  .check(
    z.refine((probes) => probes.hover.action?.type === "hover", "hover.action.type must be hover"),
    z.refine((probes) => probes.focus.action?.type === "focus", "focus.action.type must be focus"),
    z.refine(
      (probes) =>
        probes["focus-visible"].action?.type === "press" &&
        probes["focus-visible"].action.key === "Tab",
      "focus-visible.action must press Tab",
    ),
    z.refine(
      (probes) =>
        probes["responsive-below"].viewport.width < probes["responsive-above"].viewport.width,
      "responsive-below viewport must be narrower than responsive-above",
    ),
  );
const sourceSchema = z.strictObject({
  path: relativePath,
  before: nonemptyString,
  after: nonemptyString,
});
const commandSchema = z.array(nonemptyString).check(z.minLength(1));
const repositoryUrl = z.url({ protocol: /^https$/ }).check(
  z.refine((value) => {
    const url = URL.parse(value);
    return url !== null && !url.username && !url.password && !url.search && !url.hash;
  }, "repository must be an HTTPS URL without credentials, query, or fragment"),
);
const projectSchema = z.discriminatedUnion("kind", [
  z.strictObject({
    id: nonemptyString,
    kind: z.literal("controlled"),
    runtime: z.enum(controlledRuntimes),
    style: z.enum(controlledStyles),
    fixture: z.optional(relativePath),
    scope: z.optional(z.enum(["package", "workspaces"])),
    idempotency: z.optional(z.literal(true)),
    source: sourceSchema,
    probes: probesSchema,
  }),
  z.strictObject({ id: nonemptyString, kind: z.literal("smoke"), fixture: nonemptyString }),
  z.strictObject({
    id: nonemptyString,
    kind: z.literal("external"),
    repository: repositoryUrl,
    revision: z.string().check(z.regex(/^[0-9a-f]{40}$/)),
    packageManager: z.string().check(z.regex(/^(npm|pnpm)@\d+\.\d+\.\d+$/)),
    lockfile: relativePath,
    packageRoot: relativePath,
    installs: z
      .array(z.strictObject({ cwd: relativePath, args: commandSchema }))
      .check(z.minLength(1), z.maxLength(4)),
    runtimeWrites: z.array(relativePath).check(z.maxLength(3)),
    start: commandSchema.check(
      z.refine(
        (args) => args.length === 2 && args[0] === "run" && /^[a-z0-9:_-]+$/.test(args[1]),
        "start must name one reviewed package script",
      ),
    ),
    tailwindCss: relativePath,
    source: sourceSchema,
    probes: probesSchema,
  }),
]);
// Parse the envelope first so matrix cells can inherit probes before the
// complete project schema requires them. Scenario fixtures never inherit.
const manifestSchema = z.strictObject({
  matrixProbes: z.optional(controlledProbesSchema),
  projects: z.array(z.pipe(recordInput, z.record(z.string(), z.unknown()))).check(z.minLength(1)),
});

function validateProject(project: Project, index: number): void {
  const label = `projects[${index}]`;
  if (project.kind === "smoke") return;
  const probes = Object.entries(project.probes);
  if (project.kind === "controlled") {
    if (project.fixture === undefined) controlledProbesSchema.parse(project.probes);
    if (probes.every(([, probe]) => probe.witness === false)) {
      throw new Error(`${label}.probes must keep at least one causal-witness probe`);
    }
    return;
  }
  // The external lifecycle witnesses every probe; exemptions would be ignored.
  for (const [name, probe] of probes) {
    if ("witness" in probe) {
      throw new Error(`${label}.probes.${name}.witness is only supported for controlled cases`);
    }
  }
  const manager = project.packageManager.slice(0, project.packageManager.indexOf("@"));
  const lockfiles: Record<string, Set<string>> = {
    npm: new Set(["package-lock.json", "npm-shrinkwrap.json"]),
    pnpm: new Set(["pnpm-lock.yaml"]),
  };
  if (!lockfiles[manager]?.has(posix.basename(project.lockfile))) {
    throw new Error(`${label}.lockfile does not match ${manager}`);
  }
  const expected =
    manager === "npm"
      ? ["ci", "--ignore-scripts", "--no-audit", "--no-fund"]
      : ["install", "--frozen-lockfile", "--ignore-scripts"];
  project.installs.forEach(({ args }, installIndex) => {
    if (args.length !== expected.length || args.some((part, index) => part !== expected[index])) {
      throw new Error(
        `${label}.installs[${installIndex}].args must be a locked script-free install`,
      );
    }
  });
  if (new Set(project.runtimeWrites).size !== project.runtimeWrites.length) {
    throw new Error(`${label}.runtimeWrites must be unique`);
  }
  const protectedPaths = new Set([
    posix.normalize(project.lockfile),
    posix.normalize(posix.join(project.packageRoot, project.tailwindCss)),
    posix.normalize(posix.join(project.packageRoot, project.source.path)),
  ]);
  if (project.runtimeWrites.some((path) => protectedPaths.has(posix.normalize(path)))) {
    throw new Error(`${label}.runtimeWrites must not include migration or lockfile paths`);
  }
}

function validateManifest(value: unknown): Manifest {
  const manifest = manifestSchema.parse(value);
  const projects = manifest.projects.map((record) =>
    projectSchema.parse(
      manifest.matrixProbes !== undefined &&
        record.kind === "controlled" &&
        !("fixture" in record) &&
        !("probes" in record)
        ? { ...record, probes: manifest.matrixProbes }
        : record,
    ),
  );
  const ids = new Set<string>();
  const cells = new Set<string>();
  projects.forEach((project, index) => {
    validateProject(project, index);
    if (ids.has(project.id)) throw new Error(`duplicate project id ${JSON.stringify(project.id)}`);
    ids.add(project.id);
    if (project.kind === "controlled") {
      const cell = project.fixture ?? `${project.runtime}/${project.style}`;
      if (cells.has(cell))
        throw new Error(`duplicate controlled matrix cell ${JSON.stringify(cell)}`);
      cells.add(cell);
    }
  });
  for (const project of projects.filter((entry) => entry.kind === "smoke")) {
    if (!projects.some(({ id, kind }) => id === project.fixture && kind === "controlled")) {
      throw new Error(
        `smoke fixture ${JSON.stringify(project.fixture)} must reference a controlled case`,
      );
    }
  }
  return { projects };
}

export async function loadManifest(): Promise<Manifest> {
  const url = new URL("./projects.json", import.meta.url);
  return validateManifest(JSON.parse(await readFile(url, "utf8")));
}

export function vitestProjects(
  projects: Project[],
  env: NodeJS.ProcessEnv = process.env,
): Project[] {
  const externalEnabled = env.CI === "true" && env.ECOSYSTEM_EXTERNAL === "1";
  return projects.filter((project) => project.kind !== "external" || externalEnabled);
}

// Regular CI keeps compiler coverage on Vite and the distinct Next webpack/Less
// path. The full matrix still exercises every framework/compiler/OS combination.
function ciMatrix(manifest: Manifest, full: boolean) {
  const deferred = new Set(["next-scss", "next-sass", "vite-html-sass", "vite-html-less"]);
  const crossPlatform = new Set([
    "react-vite-scss",
    "react-vite-less",
    "vite-html-css",
    "media-workspace-split",
  ]);
  const platforms = { linux: "ubuntu-latest", macos: "macos-latest", windows: "windows-latest" };
  return {
    include: Object.entries(platforms).flatMap(([os, runner]) =>
      manifest.projects
        .filter(
          (project) =>
            full ||
            (os === "linux"
              ? project.kind !== "external" && !deferred.has(project.id)
              : crossPlatform.has(project.id)),
        )
        .map((project) => ({
          os,
          runner,
          case: project.id,
          external: project.kind === "external",
        })),
    ),
  };
}

function selectProjects(args: string[], manifest: Manifest): Project[] {
  if (args.length === 1 && args[0] === "--all")
    return manifest.projects.filter(({ kind }) => kind === "controlled");
  if (args.length === 2 && args[0] === "--external-case") {
    if (process.env.CI !== "true" || process.env.ECOSYSTEM_EXTERNAL !== "1") {
      throw new Error("External cases are CI-only");
    }
    const project = manifest.projects.find(({ id, kind }) => id === args[1] && kind === "external");
    if (project) return [project];
    throw new Error(`Unknown external case ${JSON.stringify(args[1])}`);
  }
  if (args.length === 2 && args[0] === "--case") {
    const project = manifest.projects.find(({ id }) => id === args[1]);
    if (project?.kind === "external")
      throw new Error(`External case ${JSON.stringify(project.id)} is CI-only`);
    if (project) return [project];
    throw new Error(
      `Unknown case ${JSON.stringify(args[1])}. Available ids: ${manifest.projects
        .filter(({ kind }) => kind !== "external")
        .map(({ id }) => id)
        .join(", ")}`,
    );
  }
  throw new Error(usage);
}

export function resolveFixture(manifest: Manifest, project: Project): ProbedProject {
  // validateManifest pins every smoke fixture to an existing controlled case.
  return project.kind === "smoke"
    ? (manifest.projects.find(({ id }) => id === project.fixture) as ControlledProject)
    : project;
}

function executeVitest(args: string[]): void {
  const pnpm = platformCommand("pnpm");
  const result = spawnSync(pnpm, ["exec", "vitest", ...args], {
    shell: pnpm.endsWith(".cmd"),
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Vitest exited with status ${result.status}`);
}

async function withLocalPackageArtifacts<T>(operation: () => T | Promise<T>): Promise<T> {
  if (process.env.ECOSYSTEM_PACKAGE_ARTIFACT_ROOT) return operation();
  const temporaryRoot = await mkdtemp(join(tmpdir(), "tw-migrate-ecosystem-packages-"));
  const artifactRoot = join(temporaryRoot, "packages");
  try {
    await stagePackages({
      repoRoot: resolve(dirname(fileURLToPath(import.meta.url)), ".."),
      artifactRoot,
    });
    process.env.ECOSYSTEM_PACKAGE_ARTIFACT_ROOT = artifactRoot;
    return operation();
  } finally {
    delete process.env.ECOSYSTEM_PACKAGE_ARTIFACT_ROOT;
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

// The CI-only allowlist step: copies only ledger-declared artifacts of the
// case into the `-upload` tree consumed by upload-artifact.
async function prepareUpload(caseId: string): Promise<void> {
  const artifactRoot = process.env.ECOSYSTEM_ARTIFACT_ROOT;
  if (!artifactRoot) throw new Error("ECOSYSTEM_ARTIFACT_ROOT is required");
  const manifest = await loadManifest();
  const project = manifest.projects.find(({ id }) => id === caseId);
  if (!project) throw new Error(`unknown case ${JSON.stringify(caseId)}`);
  await prepareCaseUpload(
    resolveFixture(manifest, project),
    artifactRoot,
    packageUploadRoot(artifactRoot),
  );
}

async function main() {
  try {
    const args = process.argv.slice(2);
    if (args.length === 2 && args[0] === "--prepare-upload") return await prepareUpload(args[1]);
    const manifest = await loadManifest();
    if (args.length === 2 && args[0] === "--ci-matrix" && ["regular", "full"].includes(args[1])) {
      console.log(JSON.stringify(ciMatrix(manifest, args[1] === "full")));
      return;
    }
    const selected = selectProjects(args, manifest);
    await withLocalPackageArtifacts(() =>
      executeVitest([
        "run",
        "--config",
        "ecosystem-ci/vite.config.ts",
        ...selected.flatMap(({ id }) => ["--project", id]),
      ]),
    );
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) await main();
