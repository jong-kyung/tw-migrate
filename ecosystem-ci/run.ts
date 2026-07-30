#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, posix, resolve, win32 } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { stagePackages } from "./packages.ts";
import { platformCommand } from "./shared.ts";
import type { ControlledProject, Manifest, ProbedProject, Project } from "./types.ts";

// `projects.json` is untrusted until validateManifest narrows it, so the
// validators below walk `unknown` values rather than the manifest types.
type Unknown = Record<string, unknown>;

const usage = "Usage: node ecosystem-ci/run.ts (--case <id> | --all)";
const runtimes = new Set(["react-vite", "next", "vite-html", "vue-vite"]);
const styles = new Set(["css", "scss", "sass", "less"]);
const selectorTypes = new Set(["role", "name", "text", "data", "id", "tag", "css"]);

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

function positiveInteger(value: unknown): boolean {
  return Number.isInteger(value) && (value as number) >= 1;
}

function nonempty(value: unknown, label: string): void {
  if (typeof value !== "string" || value.length === 0)
    throw new Error(`${label} must be a non-empty string`);
}

function validateSelector(value: unknown, label: string): void {
  const selector = exactKeys(value, ["type", "value", "name"], ["type", "value"], label);
  if (typeof selector.type !== "string" || !selectorTypes.has(selector.type)) {
    throw new Error(
      `${label}.type must be role, name, text, data, id, tag, or css (CSS class selectors are forbidden)`,
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

function validateExpectation(value: unknown, label: string): void {
  const expectation = exactKeys(
    value,
    ["selector", "cardinality"],
    ["selector", "cardinality"],
    label,
  );
  validateSelector(expectation.selector, `${label}.selector`);
  if (!positiveInteger(expectation.cardinality)) {
    throw new Error(`${label}.cardinality must be a positive integer`);
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
  } else if (["click", "hover", "focus"].includes(action.type as string)) {
    exactKeys(action, ["type", "selector"], ["type", "selector"], label);
    validateSelector(action.selector, `${label}.selector`);
  } else {
    throw new Error(`${label}.type must be click, hover, focus, or press`);
  }
}

function validateProbe(value: unknown, label: string): void {
  const probe = exactKeys(
    value,
    ["route", "viewport", "readiness", "selector", "cardinality", "identity", "action"],
    ["route", "viewport", "readiness", "selector", "cardinality", "identity"],
    label,
  );
  nonempty(probe.route, `${label}.route`);
  validateViewport(probe.viewport, `${label}.viewport`);
  validateExpectation(probe.readiness, `${label}.readiness`);
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
  const reviewedBuild =
    manager === "pnpm" &&
    args.length === 4 &&
    args[0] === "--filter" &&
    /^@?[a-z0-9][a-z0-9@/._-]*$/.test(String(args[1])) &&
    args[2] === "run" &&
    /^[a-z0-9:_-]+$/.test(String(args[3]));
  if (!lockedInstall && !reviewedBuild) {
    throw new Error(
      `${label} must be a locked script-free install or a reviewed pnpm workspace build`,
    );
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
  validateProbes(project.probes, `${label}.probes`, project.kind === "controlled");
}

function validateProject(value: unknown, index: number): void {
  const label = `projects[${index}]`;
  const project = object(value, label);
  if (project.kind === "controlled") {
    exactKeys(
      project,
      ["id", "kind", "runtime", "style", "source", "probes"],
      ["id", "kind", "runtime", "style", "source", "probes"],
      label,
    );
    if (!runtimes.has(project.runtime as string))
      throw new Error(`${label}.runtime is unsupported`);
    if (!styles.has(project.style as string)) throw new Error(`${label}.style is unsupported`);
  } else if (project.kind === "smoke") {
    exactKeys(project, ["id", "kind", "fixture"], ["id", "kind", "fixture"], label);
    nonempty(project.fixture, `${label}.fixture`);
  } else if (project.kind === "external") {
    exactKeys(
      project,
      [
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
        "server",
        "tailwindCss",
        "source",
        "probes",
      ],
      [
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
        "server",
        "tailwindCss",
        "source",
        "probes",
      ],
      label,
    );
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
    if (!["vite", "next"].includes(project.server as string))
      throw new Error(`${label}.server must be vite or next`);
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
  const manifest = exactKeys(value, ["projects"], ["projects"], "manifest");
  if (!Array.isArray(manifest.projects) || manifest.projects.length === 0) {
    throw new Error("manifest.projects must be a non-empty array");
  }

  const ids = new Set<string>();
  const cells = new Set<string>();
  manifest.projects.forEach((project: Project, index: number) => {
    validateProject(project, index);
    if (ids.has(project.id)) throw new Error(`duplicate project id ${JSON.stringify(project.id)}`);
    ids.add(project.id);
    if (project.kind === "controlled") {
      const cell = `${project.runtime}/${project.style}`;
      if (cells.has(cell))
        throw new Error(`duplicate controlled matrix cell ${JSON.stringify(cell)}`);
      cells.add(cell);
    }
  });
  const projects = manifest.projects as Project[];
  for (const project of projects.filter((entry) => entry.kind === "smoke")) {
    if (!projects.some(({ id, kind }) => id === project.fixture && kind === "controlled")) {
      throw new Error(
        `smoke fixture ${JSON.stringify(project.fixture)} must reference a controlled case`,
      );
    }
  }
  // Every field the manifest types promise has now been checked.
  return { projects } as Manifest;
}

export async function loadManifest(
  url: URL = new URL("./projects.json", import.meta.url),
): Promise<Manifest> {
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
  validateManifest(manifest);
  const selected = selectProjects(args, manifest);
  execute([
    "run",
    "--config",
    "ecosystem-ci/vitest.config.ts",
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

async function main() {
  try {
    const args = process.argv.slice(2);
    const manifest = await loadManifest();
    selectProjects(args, manifest);
    await withLocalPackageArtifacts(() => runHarness(args, manifest));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) await main();
