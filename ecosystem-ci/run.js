#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, posix, resolve, win32 } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { stagePackages } from './packages.js';

const usage = 'Usage: node ecosystem-ci/run.js (--case <id> | --all)';
const runtimes = new Set(['react-vite', 'next', 'vite-html']);
const styles = new Set(['css', 'scss', 'sass', 'less']);
const selectorTypes = new Set(['role', 'name', 'text', 'data', 'id', 'tag', 'css']);

function object(value, label) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value;
}

function exactKeys(value, allowed, required, label) {
  object(value, label);
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) throw new Error(`${label} has unknown key ${JSON.stringify(key)}`);
  }
  for (const key of required) {
    if (!(key in value)) throw new Error(`${label} is missing ${JSON.stringify(key)}`);
  }
}

function nonempty(value, label) {
  if (typeof value !== 'string' || value.length === 0) throw new Error(`${label} must be a non-empty string`);
}

function validateSelector(selector, label) {
  exactKeys(selector, ['type', 'value', 'name'], ['type', 'value'], label);
  if (!selectorTypes.has(selector.type)) {
    throw new Error(`${label}.type must be role, name, text, data, id, tag, or css (CSS class selectors are forbidden)`);
  }
  nonempty(selector.value, `${label}.value`);
  if (selector.type === 'tag' && !/^[a-z][a-z0-9-]*$/.test(selector.value)) {
    throw new Error(`${label}.value must be one lowercase HTML tag`);
  }
  if (selector.type === 'css' && /[.#]/.test(selector.value)) {
    throw new Error(`${label}.value must not contain class or id selectors`);
  }
  if ('name' in selector) {
    if (selector.type !== 'role') throw new Error(`${label}.name is only valid for role selectors`);
    nonempty(selector.name, `${label}.name`);
  }
}

function validateExpectation(expectation, label) {
  exactKeys(expectation, ['selector', 'cardinality'], ['selector', 'cardinality'], label);
  validateSelector(expectation.selector, `${label}.selector`);
  if (!Number.isInteger(expectation.cardinality) || expectation.cardinality < 1) {
    throw new Error(`${label}.cardinality must be a positive integer`);
  }
}

function validateViewport(viewport, label) {
  exactKeys(viewport, ['width', 'height'], ['width', 'height'], label);
  for (const dimension of ['width', 'height']) {
    if (!Number.isInteger(viewport[dimension]) || viewport[dimension] < 1) {
      throw new Error(`${label}.${dimension} must be a positive integer`);
    }
  }
}

function validateAction(action, label) {
  object(action, label);
  if (action.type === 'press') {
    exactKeys(action, ['type', 'key'], ['type', 'key'], label);
    nonempty(action.key, `${label}.key`);
  } else if (['click', 'hover', 'focus'].includes(action.type)) {
    exactKeys(action, ['type', 'selector'], ['type', 'selector'], label);
    validateSelector(action.selector, `${label}.selector`);
  } else {
    throw new Error(`${label}.type must be click, hover, focus, or press`);
  }
}

function validateProbe(probe, label) {
  exactKeys(
    probe,
    ['route', 'viewport', 'readiness', 'selector', 'cardinality', 'identity', 'action'],
    ['route', 'viewport', 'readiness', 'selector', 'cardinality', 'identity'],
    label,
  );
  nonempty(probe.route, `${label}.route`);
  validateViewport(probe.viewport, `${label}.viewport`);
  validateExpectation(probe.readiness, `${label}.readiness`);
  validateSelector(probe.selector, `${label}.selector`);
  if (!Number.isInteger(probe.cardinality) || probe.cardinality < 1) {
    throw new Error(`${label}.cardinality must be a positive integer`);
  }
  if (!Array.isArray(probe.identity) || probe.identity.length !== probe.cardinality) {
    throw new Error(`${label}.identity must contain one stable identity per target`);
  }
  probe.identity.forEach((identity, index) => nonempty(identity, `${label}.identity[${index}]`));
  if ('action' in probe) validateAction(probe.action, `${label}.action`);
}

function validateRelativePath(path, label) {
  nonempty(path, label);
  if (posix.isAbsolute(path) || win32.isAbsolute(path) || path.split(/[\\/]/).includes('..')) {
    throw new Error(`${label} must be a relative path without traversal`);
  }
}

function validateSource(source, label) {
  exactKeys(source, ['path', 'before', 'after'], ['path', 'before', 'after'], label);
  validateRelativePath(source.path, `${label}.path`);
  nonempty(source.before, `${label}.before`);
  nonempty(source.after, `${label}.after`);
}

function validateProbes(probes, label, controlled) {
  object(probes, label);
  if (controlled) {
    const names = ['base', 'hover', 'focus', 'focus-visible', 'responsive-below', 'responsive-above'];
    exactKeys(probes, names, names, label);
  } else if (Object.keys(probes).length === 0) {
    throw new Error(`${label} must contain at least one probe`);
  }

  for (const [name, probe] of Object.entries(probes)) validateProbe(probe, `${label}.${name}`);
  if (!controlled) return;

  for (const [name, type] of [['hover', 'hover'], ['focus', 'focus'], ['focus-visible', 'press']]) {
    if (!probes[name].action || probes[name].action.type !== type) {
      throw new Error(`${label}.${name}.action.type must be ${JSON.stringify(type)}`);
    }
  }
  if (probes['focus-visible'].action.key !== 'Tab') {
    throw new Error(`${label}.focus-visible.action.key must be "Tab"`);
  }
  if (probes['responsive-below'].viewport.width >= probes['responsive-above'].viewport.width) {
    throw new Error(`${label}.responsive-below viewport must be narrower than responsive-above`);
  }
}

function validateCommand(command, label) {
  if (!Array.isArray(command) || command.length === 0 || command.some((part) => typeof part !== 'string' || part.length === 0)) {
    throw new Error(`${label} must be a non-empty argument array, not a shell command string`);
  }
}

function validateExternalInstall(manager, args, label) {
  validateCommand(args, label);
  const exact = (expected) => args.length === expected.length && args.every((part, index) => part === expected[index]);
  const lockedInstall = manager === 'npm'
    ? exact(['ci', '--ignore-scripts', '--no-audit', '--no-fund'])
    : exact(['install', '--frozen-lockfile', '--ignore-scripts']);
  const reviewedBuild = manager === 'pnpm'
    && args.length === 4
    && args[0] === '--filter'
    && /^@?[a-z0-9][a-z0-9@/._-]*$/.test(args[1])
    && args[2] === 'run'
    && /^[a-z0-9:_-]+$/.test(args[3]);
  if (!lockedInstall && !reviewedBuild) {
    throw new Error(`${label} must be a locked script-free install or a reviewed pnpm workspace build`);
  }
}

function validateExternalStart(args, label) {
  validateCommand(args, label);
  if (args.length !== 2 || args[0] !== 'run' || !/^[a-z0-9:_-]+$/.test(args[1])) {
    throw new Error(`${label} must name one reviewed package script`);
  }
}

function validateCommon(project, label) {
  nonempty(project.id, `${label}.id`);
  validateSource(project.source, `${label}.source`);
  validateProbes(project.probes, `${label}.probes`, project.kind === 'controlled');
}

function validateProject(project, index) {
  const label = `projects[${index}]`;
  object(project, label);
  if (project.kind === 'controlled') {
    exactKeys(
      project,
      ['id', 'kind', 'runtime', 'style', 'source', 'probes'],
      ['id', 'kind', 'runtime', 'style', 'source', 'probes'],
      label,
    );
    if (!runtimes.has(project.runtime)) throw new Error(`${label}.runtime is unsupported`);
    if (!styles.has(project.style)) throw new Error(`${label}.style is unsupported`);
  } else if (project.kind === 'smoke') {
    exactKeys(project, ['id', 'kind', 'fixture'], ['id', 'kind', 'fixture'], label);
    nonempty(project.fixture, `${label}.fixture`);
  } else if (project.kind === 'external') {
    exactKeys(
      project,
      ['id', 'kind', 'repository', 'revision', 'packageManager', 'lockfile', 'packageRoot', 'installs', 'runtimeWrites', 'start', 'server', 'tailwindCss', 'source', 'probes'],
      ['id', 'kind', 'repository', 'revision', 'packageManager', 'lockfile', 'packageRoot', 'installs', 'runtimeWrites', 'start', 'server', 'tailwindCss', 'source', 'probes'],
      label,
    );
    nonempty(project.repository, `${label}.repository`);
    let repository;
    try { repository = new URL(project.repository); } catch { throw new Error(`${label}.repository must be an HTTPS URL`); }
    if (repository.protocol !== 'https:' || repository.username || repository.password || repository.search || repository.hash) {
      throw new Error(`${label}.repository must be an HTTPS URL without credentials, query, or fragment`);
    }
    if (!/^[0-9a-f]{40}$/.test(project.revision)) throw new Error(`${label}.revision must be a full 40-character SHA`);
    if (!/^(npm|pnpm)@\d+\.\d+\.\d+$/.test(project.packageManager)) {
      throw new Error(`${label}.packageManager must preserve an exact numeric npm or pnpm version`);
    }
    validateRelativePath(project.lockfile, `${label}.lockfile`);
    validateRelativePath(project.packageRoot, `${label}.packageRoot`);
    validateRelativePath(project.tailwindCss, `${label}.tailwindCss`);
    const manager = project.packageManager.slice(0, project.packageManager.indexOf('@'));
    const lockfiles = {
      npm: new Set(['package-lock.json', 'npm-shrinkwrap.json']),
      pnpm: new Set(['pnpm-lock.yaml']),
    };
    if (!lockfiles[manager].has(posix.basename(project.lockfile))) {
      throw new Error(`${label}.lockfile does not match ${manager}`);
    }
    if (!Array.isArray(project.installs) || project.installs.length === 0 || project.installs.length > 4) {
      throw new Error(`${label}.installs must contain one to four reviewed package-manager invocations`);
    }
    project.installs.forEach((install, installIndex) => {
      const installLabel = `${label}.installs[${installIndex}]`;
      exactKeys(install, ['cwd', 'args'], ['cwd', 'args'], installLabel);
      validateRelativePath(install.cwd, `${installLabel}.cwd`);
      validateExternalInstall(manager, install.args, `${installLabel}.args`);
    });
    if (!Array.isArray(project.runtimeWrites) || project.runtimeWrites.length > 3) {
      throw new Error(`${label}.runtimeWrites must be an array of at most three reviewed paths`);
    }
    project.runtimeWrites.forEach((path, pathIndex) => validateRelativePath(path, `${label}.runtimeWrites[${pathIndex}]`));
    if (new Set(project.runtimeWrites).size !== project.runtimeWrites.length) throw new Error(`${label}.runtimeWrites must be unique`);
    validateExternalStart(project.start, `${label}.start`);
    if (!['vite', 'next'].includes(project.server)) throw new Error(`${label}.server must be vite or next`);
  } else {
    throw new Error(`${label}.kind must be controlled, smoke, or external`);
  }
  if (project.kind !== 'smoke') validateCommon(project, label);
  if (project.kind === 'external') {
    const protectedPaths = new Set([
      posix.normalize(project.lockfile),
      posix.normalize(posix.join(project.packageRoot, project.tailwindCss)),
      posix.normalize(posix.join(project.packageRoot, project.source.path)),
    ]);
    if (project.runtimeWrites.some((path) => protectedPaths.has(posix.normalize(path)))) {
      throw new Error(`${label}.runtimeWrites must not include migration or lockfile paths`);
    }
  }
}

export function validateManifest(manifest) {
  exactKeys(manifest, ['projects'], ['projects'], 'manifest');
  if (!Array.isArray(manifest.projects) || manifest.projects.length === 0) {
    throw new Error('manifest.projects must be a non-empty array');
  }

  const ids = new Set();
  const cells = new Set();
  manifest.projects.forEach((project, index) => {
    validateProject(project, index);
    if (ids.has(project.id)) throw new Error(`duplicate project id ${JSON.stringify(project.id)}`);
    ids.add(project.id);
    if (project.kind === 'controlled') {
      const cell = `${project.runtime}/${project.style}`;
      if (cells.has(cell)) throw new Error(`duplicate controlled matrix cell ${JSON.stringify(cell)}`);
      cells.add(cell);
    }
  });
  for (const project of manifest.projects.filter(({ kind }) => kind === 'smoke')) {
    if (!manifest.projects.some(({ id, kind }) => id === project.fixture && kind === 'controlled')) {
      throw new Error(`smoke fixture ${JSON.stringify(project.fixture)} must reference a controlled case`);
    }
  }
  return manifest;
}

export async function loadManifest(url = new URL('./projects.json', import.meta.url)) {
  return validateManifest(JSON.parse(await readFile(url, 'utf8')));
}

export function vitestProjects(projects, env = process.env) {
  const externalEnabled = env.CI === 'true' && env.ECOSYSTEM_EXTERNAL === '1';
  return projects.filter((project) => project.kind !== 'external' || externalEnabled);
}

function selectProjects(args, manifest) {
  if (args.length === 1 && args[0] === '--all') return manifest.projects.filter(({ kind }) => kind === 'controlled');
  if (args.length === 2 && args[0] === '--external-case') {
    if (process.env.CI !== 'true' || process.env.ECOSYSTEM_EXTERNAL !== '1') {
      throw new Error('External cases are CI-only');
    }
    const project = manifest.projects.find(({ id, kind }) => id === args[1] && kind === 'external');
    if (project) return [project];
    throw new Error(`Unknown external case ${JSON.stringify(args[1])}`);
  }
  if (args.length === 2 && args[0] === '--case') {
    const project = manifest.projects.find(({ id }) => id === args[1]);
    if (project?.kind === 'external') throw new Error(`External case ${JSON.stringify(project.id)} is CI-only`);
    if (project) return [project];
    throw new Error(`Unknown case ${JSON.stringify(args[1])}. Available ids: ${manifest.projects.filter(({ kind }) => kind !== 'external').map(({ id }) => id).join(', ')}`);
  }
  throw new Error(usage);
}

export function resolveFixture(manifest, project) {
  return project.kind === 'smoke'
    ? manifest.projects.find(({ id }) => id === project.fixture)
    : project;
}

export function runHarness(args, manifest, execute = executeVitest) {
  validateManifest(manifest);
  const selected = selectProjects(args, manifest);
  execute(['run', '--config', 'ecosystem-ci/vitest.config.js', ...selected.flatMap(({ id }) => ['--project', id])]);
  return selected;
}

function executeVitest(args) {
  const result = spawnSync(process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm', ['exec', 'vitest', ...args], { stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Vitest exited with status ${result.status}`);
}

async function withLocalPackageArtifacts(operation) {
  if (process.env.ECOSYSTEM_PACKAGE_ARTIFACT_ROOT) return operation();
  const temporaryRoot = await mkdtemp(join(tmpdir(), 'tw-migrate-ecosystem-packages-'));
  const artifactRoot = join(temporaryRoot, 'packages');
  try {
    await stagePackages({
      repoRoot: resolve(dirname(fileURLToPath(import.meta.url)), '..'),
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
    console.error(error.message);
    process.exitCode = 1;
  }
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) await main();
