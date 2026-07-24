#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

const usage = 'Usage: node ecosystem-ci/run.js (--case <id> | --all)';
const runtimes = new Set(['react-vite', 'next', 'vite-html']);
const styles = new Set(['css', 'scss', 'sass', 'less']);
const selectorTypes = new Set(['role', 'name', 'text', 'data', 'id']);

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
    throw new Error(`${label}.type must be role, name, text, data, or id (CSS class selectors are forbidden)`);
  }
  nonempty(selector.value, `${label}.value`);
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

function validateSource(source, label) {
  exactKeys(source, ['path', 'before', 'after'], ['path', 'before', 'after'], label);
  nonempty(source.path, `${label}.path`);
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
    throw new Error(`${label} must be a non-empty argv array, not a shell command string`);
  }
  if (['sh', 'bash', 'zsh', 'cmd', 'powershell', 'pwsh'].includes(command[0])) {
    throw new Error(`${label} must not invoke a shell`);
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
    exactKeys(
      project,
      ['id', 'kind', 'source', 'probes'],
      ['id', 'kind', 'source', 'probes'],
      label,
    );
  } else if (project.kind === 'external') {
    exactKeys(
      project,
      ['id', 'kind', 'repository', 'revision', 'packageManager', 'lockfile', 'install', 'start', 'source', 'probes'],
      ['id', 'kind', 'repository', 'revision', 'packageManager', 'lockfile', 'install', 'start', 'source', 'probes'],
      label,
    );
    nonempty(project.repository, `${label}.repository`);
    if (!/^[0-9a-f]{40}$/.test(project.revision)) throw new Error(`${label}.revision must be a full 40-character SHA`);
    if (!/^(npm|pnpm|yarn|bun)@[^\s]+$/.test(project.packageManager)) {
      throw new Error(`${label}.packageManager must preserve an exact package-manager version`);
    }
    nonempty(project.lockfile, `${label}.lockfile`);
    const manager = project.packageManager.slice(0, project.packageManager.indexOf('@'));
    const lockfiles = {
      npm: new Set(['package-lock.json', 'npm-shrinkwrap.json']),
      pnpm: new Set(['pnpm-lock.yaml']),
      yarn: new Set(['yarn.lock']),
      bun: new Set(['bun.lock', 'bun.lockb']),
    };
    if (!lockfiles[manager].has(project.lockfile)) {
      throw new Error(`${label}.lockfile does not match ${manager}`);
    }
    validateCommand(project.install, `${label}.install`);
    validateCommand(project.start, `${label}.start`);
  } else {
    throw new Error(`${label}.kind must be controlled, smoke, or external`);
  }
  validateCommon(project, label);
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
  return manifest;
}

export async function loadManifest(url = new URL('./projects.json', import.meta.url)) {
  return validateManifest(JSON.parse(await readFile(url, 'utf8')));
}

export function ecosystemMatrix(manifest) {
  validateManifest(manifest);
  const runners = { linux: 'ubuntu-latest', macos: 'macos-latest', windows: 'windows-latest' };
  return Object.entries(runners).flatMap(([os, runner]) => manifest.projects.map((project) => ({ os, runner, case: project.id })));
}

function selectProjects(args, manifest) {
  if (args.length === 1 && args[0] === '--all') return manifest.projects;
  if (args.length === 2 && args[0] === '--case') {
    const project = manifest.projects.find(({ id }) => id === args[1]);
    if (project) return [project];
    throw new Error(`Unknown case ${JSON.stringify(args[1])}. Available ids: ${manifest.projects.map(({ id }) => id).join(', ')}`);
  }
  throw new Error(usage);
}

export function runHarness(args, manifest, execute = executeVitest) {
  validateManifest(manifest);
  const selected = selectProjects(args, manifest);
  execute(args[0] === '--all'
    ? ['run', '--config', 'ecosystem-ci/vitest.config.js']
    : ['run', '--config', 'ecosystem-ci/vitest.config.js', '--project', selected[0].id]);
  return selected;
}

function executeVitest(args) {
  const result = spawnSync(process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm', ['exec', 'vitest', ...args], { stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Vitest exited with status ${result.status}`);
}

async function main() {
  try {
    runHarness(process.argv.slice(2), await loadManifest());
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) await main();
