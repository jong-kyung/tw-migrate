#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, posix, resolve, win32 } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import * as v from "valibot";

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
const nonemptyString = v.pipe(v.string(), v.minLength(1));
// Keep raw keys until strict project validation so unknown keys cannot disappear.
const recordInput = v.pipe(
  v.custom<Record<string, unknown>>(
    (value) => value !== null && typeof value === "object" && !Array.isArray(value),
    "Expected an object",
  ),
  v.check((value) => !Object.hasOwn(value, "__proto__"), "Object keys must not include __proto__"),
);
// Preserve the existing integer contract without imposing a new safe-integer ceiling.
const positiveInteger = v.pipe(
  v.number(),
  v.check((value) => Number.isInteger(value), "must be an integer"),
  v.minValue(1),
);
const relativePath = v.pipe(
  nonemptyString,
  v.check(
    (path) =>
      !posix.isAbsolute(path) && !win32.isAbsolute(path) && !path.split(/[\\/]/).includes(".."),
    "must be a relative path without traversal",
  ),
);
const selectorSchema = v.pipe(
  v.strictObject({
    type: v.picklist(knownSelectorTypes),
    value: nonemptyString,
    name: v.optional(nonemptyString),
  }),
  v.check(
    (selector) => selector.type !== "tag" || /^[a-z][a-z0-9-]*$/.test(selector.value),
    "tag selector must be one lowercase HTML tag",
  ),
  v.check(
    (selector) => selector.type !== "css" || !/[.#]/.test(selector.value),
    "CSS selector must not contain class or id selectors",
  ),
  v.check(
    (selector) => selector.name === undefined || selector.type === "role",
    "name is only valid for role selectors",
  ),
);
const actionSchema = v.variant("type", [
  v.strictObject({ type: v.literal("press"), key: nonemptyString }),
  v.strictObject({ type: v.picklist(["hover", "focus"]), selector: selectorSchema }),
]);
const probeSchema = v.pipe(
  v.strictObject({
    route: nonemptyString,
    viewport: v.strictObject({ width: positiveInteger, height: positiveInteger }),
    readiness: v.strictObject({ selector: selectorSchema, cardinality: positiveInteger }),
    selector: selectorSchema,
    cardinality: positiveInteger,
    identity: v.array(nonemptyString),
    action: v.optional(actionSchema),
    witness: v.optional(v.literal(false)),
  }),
  v.check(
    (probe) => probe.identity.length === probe.cardinality,
    "identity must contain one stable identity per target",
  ),
);
const probesSchema = v.pipe(
  recordInput,
  // Validate entries without dropping probe names such as constructor or prototype.
  v.transform((value) => Object.entries(value)),
  v.array(v.tuple([v.string(), probeSchema])),
  v.minLength(1),
  v.transform((entries) => Object.fromEntries(entries)),
);
const controlledProbesSchema = v.pipe(
  v.strictObject({
    base: probeSchema,
    hover: probeSchema,
    focus: probeSchema,
    "focus-visible": probeSchema,
    "responsive-below": probeSchema,
    "responsive-above": probeSchema,
    // Only fixtures exercising <style module> need this additional observation.
    "module-panel": v.optional(probeSchema),
  }),
  v.check((probes) => probes.hover.action?.type === "hover", "hover.action.type must be hover"),
  v.check((probes) => probes.focus.action?.type === "focus", "focus.action.type must be focus"),
  v.check(
    (probes) =>
      probes["focus-visible"].action?.type === "press" &&
      probes["focus-visible"].action.key === "Tab",
    "focus-visible.action must press Tab",
  ),
  v.check(
    (probes) =>
      probes["responsive-below"].viewport.width < probes["responsive-above"].viewport.width,
    "responsive-below viewport must be narrower than responsive-above",
  ),
);
const sourceSchema = v.strictObject({
  path: relativePath,
  before: nonemptyString,
  after: nonemptyString,
});
const commandSchema = v.pipe(v.array(nonemptyString), v.minLength(1));
const repositoryUrl = v.pipe(
  v.string(),
  v.check((value) => {
    const url = URL.parse(value.trim());
    return url?.protocol === "https:" && !url.username && !url.password && !url.search && !url.hash;
  }, "repository must be an HTTPS URL without credentials, query, or fragment"),
  v.transform((value) => value.trim().replace(/[\t\n\r]/g, "")),
);
const projectSchema = v.variant("kind", [
  v.strictObject({
    id: nonemptyString,
    kind: v.literal("controlled"),
    runtime: v.picklist(controlledRuntimes),
    style: v.picklist(controlledStyles),
    fixture: v.optional(relativePath),
    scope: v.optional(v.picklist(["package", "workspaces"])),
    idempotency: v.optional(v.literal(true)),
    source: sourceSchema,
    probes: probesSchema,
  }),
  v.strictObject({ id: nonemptyString, kind: v.literal("smoke"), fixture: nonemptyString }),
  v.strictObject({
    id: nonemptyString,
    kind: v.literal("external"),
    repository: repositoryUrl,
    revision: v.pipe(v.string(), v.regex(/^[0-9a-f]{40}$/)),
    packageManager: v.pipe(v.string(), v.regex(/^(npm|pnpm)@\d+\.\d+\.\d+$/)),
    lockfile: relativePath,
    packageRoot: relativePath,
    installs: v.pipe(
      v.array(v.strictObject({ cwd: relativePath, args: commandSchema })),
      v.minLength(1),
      v.maxLength(4),
    ),
    runtimeWrites: v.pipe(v.array(relativePath), v.maxLength(3)),
    start: v.pipe(
      commandSchema,
      v.check(
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
const manifestSchema = v.strictObject({
  matrixProbes: v.optional(controlledProbesSchema),
  projects: v.pipe(v.array(recordInput), v.minLength(1)),
});

function validateProject(project: Project, index: number): void {
  const label = `projects[${index}]`;
  if (project.kind === "smoke") return;
  const probes = Object.entries(project.probes);
  if (project.kind === "controlled") {
    if (project.fixture === undefined) v.parse(controlledProbesSchema, project.probes);
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
  const manifest = v.parse(manifestSchema, value);
  const projects = manifest.projects.map((record) =>
    v.parse(
      projectSchema,
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
