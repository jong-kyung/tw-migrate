#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, posix, resolve, win32 } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { prepareCaseUpload } from "./lifecycle.ts";
import { packageUploadRoot, stagePackages } from "./packages.ts";
import { platformCommand } from "./shared.ts";
import {
  controlledRuntimes,
  controlledStyles,
  selectorTypes as knownSelectorTypes,
} from "./types.ts";
import type { ControlledProject, Manifest, ProbedProject, Project } from "./types.ts";

// `projects.json` is untrusted until validateManifest narrows it, so the
// validators below walk `unknown` values rather than the manifest types.
type Unknown = Record<string, unknown>;

const usage = "Usage: node ecosystem-ci/run.ts (--case <id> | --all)";
const runtimes = new Set<string>(controlledRuntimes);
const styles = new Set<string>(controlledStyles);
const selectorTypes = new Set<string>(knownSelectorTypes);

function object(value: unknown, label: string): Unknown {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value as Unknown;
}

function exactKeys(value: unknown, allowed: string[], required: string[], label: string): Unknown {
  const record = object(value, label);
  for (const key of Object.keys(record)) {
    if (!allowed.includes(key)) throw new Error(`${label} has unknown key ${JSON.stringify(key)}`);
  }
  for (const key of required) {
    if (!(key in record)) throw new Error(`${label} is missing ${JSON.stringify(key)}`);
  }
  return record;
}

function nonempty(value: unknown, label: string): void {
  if (typeof value !== "string" || value.length === 0)
    throw new Error(`${label} must be a non-empty string`);
}

function positiveInteger(value: unknown): boolean {
  return Number.isInteger(value) && (value as number) >= 1;
}

function validateSelector(value: unknown, label: string): void {
  const selector = exactKeys(value, ["type", "value", "name"], ["type", "value"], label);
  if (typeof selector.type !== "string" || !selectorTypes.has(selector.type)) {
    throw new Error(
      `${label}.type must be role, text, data, id, tag, or css (CSS class selectors are forbidden)`,
    );
  }
  nonempty(selector.value, `${label}.value`);
  if (selector.type === "tag" && !/^[a-z][a-z0-9-]*$/.test(String(selector.value))) {
    throw new Error(`${label}.value must be one lowercase HTML tag`);
  }
  if (selector.type === "css" && /[.#]/.test(String(selector.value))) {
    throw new Error(`${label}.value must not contain class or id selectors`);
  }
  if ("name" in selector) {
    if (selector.type !== "role") throw new Error(`${label}.name is only valid for role selectors`);
    nonempty(selector.name, `${label}.name`);
  }
}

function validateViewport(value: unknown, label: string): void {
  const viewport = exactKeys(value, ["width", "height"], ["width", "height"], label);
  for (const dimension of ["width", "height"] as const) {
    if (!positiveInteger(viewport[dimension])) {
      throw new Error(`${label}.${dimension} must be a positive integer`);
    }
  }
}

function validateAction(value: unknown, label: string): void {
  const action = object(value, label);
  if (action.type === "press") {
    exactKeys(action, ["type", "key"], ["type", "key"], label);
    nonempty(action.key, `${label}.key`);
  } else if (action.type === "hover" || action.type === "focus") {
    exactKeys(action, ["type", "selector"], ["type", "selector"], label);
    validateSelector(action.selector, `${label}.selector`);
  } else {
    throw new Error(`${label}.type must be hover, focus, or press`);
  }
}

function validateProbe(value: unknown, label: string): void {
  const probe = exactKeys(
    value,
    ["route", "viewport", "readiness", "selector", "cardinality", "identity", "action", "witness"],
    ["route", "viewport", "readiness", "selector", "cardinality", "identity"],
    label,
  );
  if ("witness" in probe && probe.witness !== false) {
    throw new Error(`${label}.witness may only be false`);
  }
  nonempty(probe.route, `${label}.route`);
  validateViewport(probe.viewport, `${label}.viewport`);
  const readiness = exactKeys(
    probe.readiness,
    ["selector", "cardinality"],
    ["selector", "cardinality"],
    `${label}.readiness`,
  );
  validateSelector(readiness.selector, `${label}.readiness.selector`);
  if (!positiveInteger(readiness.cardinality)) {
    throw new Error(`${label}.readiness.cardinality must be a positive integer`);
  }
  validateSelector(probe.selector, `${label}.selector`);
  if (!positiveInteger(probe.cardinality)) {
    throw new Error(`${label}.cardinality must be a positive integer`);
  }
  if (!Array.isArray(probe.identity) || probe.identity.length !== probe.cardinality) {
    throw new Error(`${label}.identity must contain one stable identity per target`);
  }
  probe.identity.forEach((identity: unknown, index: number) =>
    nonempty(identity, `${label}.identity[${index}]`),
  );
  if ("action" in probe) validateAction(probe.action, `${label}.action`);
}

function validateProbes(value: unknown, label: string, controlled: boolean): void {
  const probes = object(value, label);
  if (controlled) {
    const names = [
      "base",
      "hover",
      "focus",
      "focus-visible",
      "responsive-below",
      "responsive-above",
    ];
    // `module-panel` is optional: only fixtures exercising `<style module>`
    // migration carry a CSS Modules element to observe.
    exactKeys(probes, [...names, "module-panel"], names, label);
  } else if (Object.keys(probes).length === 0) {
    throw new Error(`${label} must contain at least one probe`);
  }

  for (const [name, probe] of Object.entries(probes)) validateProbe(probe, `${label}.${name}`);
  if (!controlled) return;

  const probe = (name: string) =>
    probes[name] as { action?: { type?: string; key?: string }; viewport: { width: number } };
  for (const [name, type] of [
    ["hover", "hover"],
    ["focus", "focus"],
    ["focus-visible", "press"],
  ] as const) {
    if (!probe(name).action || probe(name).action?.type !== type) {
      throw new Error(`${label}.${name}.action.type must be ${JSON.stringify(type)}`);
    }
  }
  if (probe("focus-visible").action?.key !== "Tab") {
    throw new Error(`${label}.focus-visible.action.key must be "Tab"`);
  }
  if (probe("responsive-below").viewport.width >= probe("responsive-above").viewport.width) {
    throw new Error(`${label}.responsive-below viewport must be narrower than responsive-above`);
  }
}

function validateRelativePath(value: unknown, label: string): void {
  nonempty(value, label);
  const path = value as string;
  if (posix.isAbsolute(path) || win32.isAbsolute(path) || path.split(/[\\/]/).includes("..")) {
    throw new Error(`${label} must be a relative path without traversal`);
  }
}

function validateSource(value: unknown, label: string): void {
  const source = exactKeys(value, ["path", "before", "after"], ["path", "before", "after"], label);
  validateRelativePath(source.path, `${label}.path`);
  nonempty(source.before, `${label}.before`);
  nonempty(source.after, `${label}.after`);
}

function validateCommand(command: unknown, label: string): void {
  if (
    !Array.isArray(command) ||
    command.length === 0 ||
    command.some((part: unknown) => typeof part !== "string" || part.length === 0)
  ) {
    throw new Error(`${label} must be a non-empty argument array, not a shell command string`);
  }
}

function validateExternalInstall(manager: string, value: unknown, label: string): void {
  validateCommand(value, label);
  const args = value as string[];
  const exact = (expected: string[]) =>
    args.length === expected.length && args.every((part, index) => part === expected[index]);
  const lockedInstall =
    manager === "npm"
      ? exact(["ci", "--ignore-scripts", "--no-audit", "--no-fund"])
      : exact(["install", "--frozen-lockfile", "--ignore-scripts"]);
  if (!lockedInstall) {
    throw new Error(`${label} must be a locked script-free install`);
  }
}

function validateExternalStart(value: unknown, label: string): void {
  validateCommand(value, label);
  const args = value as string[];
  if (args.length !== 2 || args[0] !== "run" || !/^[a-z0-9:_-]+$/.test(String(args[1]))) {
    throw new Error(`${label} must name one reviewed package script`);
  }
}

function validateCommon(project: Unknown, label: string): void {
  nonempty(project.id, `${label}.id`);
  validateSource(project.source, `${label}.source`);
  // The fixed probe-name matrix belongs to the runtime/style matrix
  // cells; scenario fixtures declare their own probe set like external
  // cases do.
  validateProbes(
    project.probes,
    `${label}.probes`,
    project.kind === "controlled" && !("fixture" in project),
  );
  const probes = Object.entries(object(project.probes, `${label}.probes`));
  if (
    project.kind === "controlled" &&
    probes.every(([, probe]) => object(probe, `${label}.probes`).witness === false)
  ) {
    throw new Error(`${label}.probes must keep at least one causal-witness probe`);
  }
  // The external lifecycle runs the causal witness for every probe, so a
  // witness exemption there would be silently ignored rather than honored.
  for (const [name, probe] of probes) {
    if (project.kind !== "controlled" && "witness" in object(probe, `${label}.probes`)) {
      throw new Error(`${label}.probes.${name}.witness is only supported for controlled cases`);
    }
  }
}

function validateProject(value: unknown, index: number): void {
  const label = `projects[${index}]`;
  const project = object(value, label);
  if (project.kind === "controlled") {
    exactKeys(
      project,
      ["id", "kind", "runtime", "style", "fixture", "scope", "source", "probes"],
      ["id", "kind", "runtime", "style", "source", "probes"],
      label,
    );
    if (!runtimes.has(project.runtime as string))
      throw new Error(`${label}.runtime is unsupported`);
    if (!styles.has(project.style as string)) throw new Error(`${label}.style is unsupported`);
    if ("fixture" in project) {
      nonempty(project.fixture, `${label}.fixture`);
      validateRelativePath(project.fixture, `${label}.fixture`);
    }
    if ("scope" in project && project.scope !== "package" && project.scope !== "workspaces")
      throw new Error(`${label}.scope must be "package" or "workspaces"`);
  } else if (project.kind === "smoke") {
    exactKeys(project, ["id", "kind", "fixture"], ["id", "kind", "fixture"], label);
    nonempty(project.fixture, `${label}.fixture`);
  } else if (project.kind === "external") {
    const externalKeys = [
      "id",
      "kind",
      "repository",
      "revision",
      "packageManager",
      "lockfile",
      "packageRoot",
      "installs",
      "runtimeWrites",
      "start",
      "tailwindCss",
      "source",
      "probes",
    ];
    exactKeys(project, externalKeys, externalKeys, label);
    nonempty(project.repository, `${label}.repository`);
    let repository: URL;
    try {
      repository = new URL(project.repository as string);
    } catch {
      throw new Error(`${label}.repository must be an HTTPS URL`);
    }
    if (
      repository.protocol !== "https:" ||
      repository.username ||
      repository.password ||
      repository.search ||
      repository.hash
    ) {
      throw new Error(
        `${label}.repository must be an HTTPS URL without credentials, query, or fragment`,
      );
    }
    if (!/^[0-9a-f]{40}$/.test(String(project.revision)))
      throw new Error(`${label}.revision must be a full 40-character SHA`);
    if (!/^(npm|pnpm)@\d+\.\d+\.\d+$/.test(String(project.packageManager))) {
      throw new Error(`${label}.packageManager must preserve an exact numeric npm or pnpm version`);
    }
    validateRelativePath(project.lockfile, `${label}.lockfile`);
    validateRelativePath(project.packageRoot, `${label}.packageRoot`);
    validateRelativePath(project.tailwindCss, `${label}.tailwindCss`);
    const packageManager = project.packageManager as string;
    const manager = packageManager.slice(0, packageManager.indexOf("@"));
    const lockfiles: Record<string, Set<string>> = {
      npm: new Set(["package-lock.json", "npm-shrinkwrap.json"]),
      pnpm: new Set(["pnpm-lock.yaml"]),
    };
    if (!lockfiles[manager]?.has(posix.basename(String(project.lockfile)))) {
      throw new Error(`${label}.lockfile does not match ${manager}`);
    }
    if (
      !Array.isArray(project.installs) ||
      project.installs.length === 0 ||
      project.installs.length > 4
    ) {
      throw new Error(
        `${label}.installs must contain one to four reviewed package-manager invocations`,
      );
    }
    project.installs.forEach((value: unknown, installIndex: number) => {
      const installLabel = `${label}.installs[${installIndex}]`;
      const install = exactKeys(value, ["cwd", "args"], ["cwd", "args"], installLabel);
      validateRelativePath(install.cwd, `${installLabel}.cwd`);
      validateExternalInstall(manager, install.args, `${installLabel}.args`);
    });
    if (!Array.isArray(project.runtimeWrites) || project.runtimeWrites.length > 3) {
      throw new Error(`${label}.runtimeWrites must be an array of at most three reviewed paths`);
    }
    project.runtimeWrites.forEach((path: unknown, pathIndex: number) =>
      validateRelativePath(path, `${label}.runtimeWrites[${pathIndex}]`),
    );
    if (new Set(project.runtimeWrites).size !== project.runtimeWrites.length)
      throw new Error(`${label}.runtimeWrites must be unique`);
    validateExternalStart(project.start, `${label}.start`);
  } else {
    throw new Error(`${label}.kind must be controlled, smoke, or external`);
  }
  if (project.kind !== "smoke") validateCommon(project, label);
  if (project.kind === "external") {
    const source = project.source as { path: string };
    const protectedPaths = new Set([
      posix.normalize(String(project.lockfile)),
      posix.normalize(posix.join(String(project.packageRoot), String(project.tailwindCss))),
      posix.normalize(posix.join(String(project.packageRoot), source.path)),
    ]);
    if (
      (project.runtimeWrites as string[]).some((path) => protectedPaths.has(posix.normalize(path)))
    ) {
      throw new Error(`${label}.runtimeWrites must not include migration or lockfile paths`);
    }
  }
}

export function validateManifest(value: unknown): Manifest {
  const manifest = exactKeys(value, ["matrixProbes", "projects"], ["projects"], "manifest");
  if (manifest.matrixProbes !== undefined) {
    validateProbes(manifest.matrixProbes, "manifest.matrixProbes", true);
  }
  if (!Array.isArray(manifest.projects) || manifest.projects.length === 0) {
    throw new Error("manifest.projects must be a non-empty array");
  }

  const projects = manifest.projects.map((project, index) => {
    const record = object(project, `projects[${index}]`);
    return manifest.matrixProbes !== undefined &&
      record.kind === "controlled" &&
      !("fixture" in record) &&
      !("probes" in record)
      ? { ...record, probes: manifest.matrixProbes }
      : project;
  });
  const ids = new Set<string>();
  const cells = new Set<string>();
  projects.forEach((value, index) => {
    validateProject(value, index);
    const project = value as Project;
    if (ids.has(project.id)) throw new Error(`duplicate project id ${JSON.stringify(project.id)}`);
    ids.add(project.id);
    if (project.kind === "controlled") {
      const cell = project.fixture ?? `${project.runtime}/${project.style}`;
      if (cells.has(cell))
        throw new Error(`duplicate controlled matrix cell ${JSON.stringify(cell)}`);
      cells.add(cell);
    }
  });
  const validatedProjects = projects as Project[];
  for (const project of validatedProjects.filter((entry) => entry.kind === "smoke")) {
    if (
      !validatedProjects.some(({ id, kind }) => id === project.fixture && kind === "controlled")
    ) {
      throw new Error(
        `smoke fixture ${JSON.stringify(project.fixture)} must reference a controlled case`,
      );
    }
  }
  // Every field the manifest types promise has now been checked.
  return { projects: validatedProjects };
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

export function runHarness(
  args: string[],
  manifest: Manifest,
  execute: (args: string[]) => void = executeVitest,
): Project[] {
  const selected = selectProjects(args, manifest);
  execute([
    "run",
    "--config",
    "ecosystem-ci/vite.config.ts",
    ...selected.flatMap(({ id }) => ["--project", id]),
  ]);
  return selected;
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
    selectProjects(args, manifest);
    await withLocalPackageArtifacts(() => runHarness(args, manifest));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) await main();
