import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { loadManifest, runHarness, validateManifest } from '../ecosystem-ci/run.js';

const selector = { type: 'role', value: 'button', name: 'Toggle details' };
const desktop = { width: 1280, height: 720 };
const mobile = { width: 375, height: 667 };

function probe(overrides = {}) {
  return {
    route: '/',
    viewport: desktop,
    readiness: { selector, cardinality: 1 },
    selector: { type: 'data', value: 'card' },
    cardinality: 1,
    ...overrides,
  };
}

function controlled(overrides = {}) {
  return {
    id: 'react-vite-css',
    kind: 'controlled',
    runtime: 'react-vite',
    style: 'css',
    source: { path: 'src/App.css', before: '.card', after: 'p-4' },
    probes: {
      base: probe(),
      hover: probe({ action: { type: 'hover', selector } }),
      focus: probe({ action: { type: 'focus', selector } }),
      'focus-visible': probe({ action: { type: 'press', key: 'Tab' } }),
      'responsive-below': probe({ viewport: mobile }),
      'responsive-above': probe(),
    },
    ...overrides,
  };
}

function manifest(...projects) {
  return { projects };
}

function errorFor(projects) {
  assert.throws(() => validateManifest(manifest(...projects)));
}

test('admits only the initial three controlled CSS cells', async () => {
  const loaded = await loadManifest();
  assert.deepEqual(
    loaded.projects.map(({ id, kind, runtime, style }) => [id, kind, runtime, style]),
    [
      ['react-vite-css', 'controlled', 'react-vite', 'css'],
      ['next-css', 'controlled', 'next', 'css'],
      ['vite-html-css', 'controlled', 'vite-html', 'css'],
    ],
  );
  assert.ok(loaded.projects.every((project) => !('readiness' in project)));
  assert.doesNotThrow(() => validateManifest(loaded));
});

test('smoke and external cases accept non-exhaustive probes without occupying controlled matrix cells', () => {
  const base = controlled();
  const probeFields = {
    source: base.source,
    probes: {
      base: probe(),
      details: probe({ action: { type: 'click', selector } }),
    },
  };
  assert.doesNotThrow(() =>
    validateManifest(
      manifest(
        base,
        { id: 'smoke', kind: 'smoke', ...probeFields },
        {
          id: 'external',
          kind: 'external',
          repository: 'https://example.test/project.git',
          revision: '0123456789abcdef0123456789abcdef01234567',
          packageManager: 'pnpm@10.0.0',
          lockfile: 'pnpm-lock.yaml',
          install: ['pnpm', 'install', '--frozen-lockfile'],
          start: ['pnpm', 'dev'],
          ...probeFields,
        },
      ),
    ),
  );
});

test('controlled cases require independent hover, focus, and keyboard focus-visible probes', () => {
  for (const name of ['hover', 'focus', 'focus-visible']) {
    const probes = structuredClone(controlled().probes);
    delete probes[name];
    assert.throws(() => validateManifest(manifest(controlled({ probes }))), new RegExp(`missing.*${name}`));
  }
});

test('controlled cases require responsive probes below and above the breakpoint', () => {
  for (const name of ['responsive-below', 'responsive-above']) {
    const probes = structuredClone(controlled().probes);
    delete probes[name];
    assert.throws(() => validateManifest(manifest(controlled({ probes }))), new RegExp(`missing.*${name}`));
  }
});

test('every probe requires route, viewport, readiness, selector, and cardinality', () => {
  for (const field of ['route', 'viewport', 'readiness', 'selector', 'cardinality']) {
    const project = structuredClone(controlled());
    delete project.probes.base[field];
    errorFor([project]);
  }
});

test('controlled state actions and responsive ordering are strict', () => {
  const wrongHover = structuredClone(controlled());
  wrongHover.probes.hover.action.type = 'focus';
  errorFor([wrongHover]);

  const wrongKey = structuredClone(controlled());
  wrongKey.probes['focus-visible'].action.key = 'Enter';
  errorFor([wrongKey]);

  const unordered = structuredClone(controlled());
  unordered.probes['responsive-below'].viewport.width = desktop.width;
  errorFor([unordered]);
});

test('rejects invalid manifests before execution', () => {
  errorFor([controlled(), controlled({ style: 'scss' })]);
  errorFor([controlled({ extra: true })]);
  errorFor([controlled(), controlled({ id: 'same-cell' })]);
  errorFor([
    {
      id: 'external',
      kind: 'external',
      repository: 'https://example.test/project.git',
      revision: 'abc123',
      packageManager: 'npm@11.0.0',
      lockfile: 'package-lock.json',
      install: ['npm', 'ci'],
      start: ['npm', 'start'],
      source: controlled().source,
      probes: controlled().probes,
    },
  ]);
  errorFor([
    controlled({
      probes: {
        ...controlled().probes,
        base: probe({ selector: { type: 'class', value: '.ready' } }),
      },
    }),
  ]);
  errorFor([
    controlled({
      probes: {
        ...controlled().probes,
        base: probe({ readiness: { selector, cardinality: 0 } }),
      },
    }),
  ]);
  const missingSource = controlled();
  delete missingSource.source;
  errorFor([missingSource]);
});

test('external commands must be argv arrays rather than shell strings', () => {
  const base = controlled();
  errorFor([
    {
      id: 'external',
      kind: 'external',
      repository: 'https://example.test/project.git',
      revision: '0123456789abcdef0123456789abcdef01234567',
      packageManager: 'pnpm@10.0.0',
      lockfile: 'pnpm-lock.yaml',
      install: 'pnpm install',
      start: ['pnpm', 'dev'],
      source: base.source,
      probes: { base: base.probes.base },
    },
  ]);
});

test('package script exposes the focused ecosystem harness entrypoint', () => {
  const packageJson = JSON.parse(readFileSync(new URL('../package.json', import.meta.url)));
  assert.equal(packageJson.scripts['test:ecosystem'], 'node ecosystem-ci/run.js');
});

test('--case selects exactly one project and maps it to a Vitest project filter', async () => {
  const loaded = await loadManifest();
  const calls = [];
  const selected = runHarness(['--case', 'react-vite-css'], loaded, (args) => calls.push(args));
  assert.deepEqual(selected.map(({ id }) => id), ['react-vite-css']);
  assert.deepEqual(calls, [['run', '--project', 'react-vite-css']]);
});

test('unknown case prints the available ids without executing Vitest', async () => {
  const loaded = await loadManifest();
  const message = /Unknown case "missing".*react-vite-css.*next-css.*vite-html-css/;
  assert.throws(
    () => runHarness(['--case', 'missing'], loaded, () => assert.fail('must not execute')),
    message,
  );
  assert.throws(
    () => execFileSync(process.execPath, ['ecosystem-ci/run.js', '--case', 'missing'], { encoding: 'utf8', stdio: 'pipe' }),
    (error) => error.status === 1 && message.test(error.stderr),
  );
});

test('no arguments print usage and --all is the only full-run selection', async () => {
  const loaded = await loadManifest();
  assert.throws(() => runHarness([], loaded, () => assert.fail('must not execute')), /Usage:/);
  const calls = [];
  assert.equal(runHarness(['--all'], loaded, (args) => calls.push(args)).length, 3);
  assert.deepEqual(calls, [['run']]);
});

test('the no-argument CLI stays browser-free and returns usage', () => {
  assert.throws(
    () => execFileSync(process.execPath, ['ecosystem-ci/run.js'], { encoding: 'utf8', stdio: 'pipe' }),
    (error) => error.status === 1 && /Usage:/.test(error.stderr),
  );
});
