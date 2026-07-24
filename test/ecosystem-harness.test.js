import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  assertInstalledLayout,
  currentTarget,
  stageRootPackage,
  validateProvenance,
} from '../ecosystem-ci/packages.js';
import { registryConfig } from '../ecosystem-ci/registry.js';
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

test('stages concrete optional dependency versions without changing the tracked manifest', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'tw-migrate-stage-test-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const repoRoot = join(root, 'repo');
  const stageRoot = join(root, 'stage');
  await mkdir(repoRoot);
  const tracked = `${JSON.stringify({
    name: 'tw-migrate',
    version: '1.2.3',
    files: ['index.js'],
    optionalDependencies: {
      'tw-migrate-darwin-arm64': 'workspace:1.2.3',
      'tw-migrate-linux-x64-gnu': 'workspace:1.2.3',
    },
  }, null, 2)}\n`;
  await Promise.all([
    writeFile(join(repoRoot, 'package.json'), tracked),
    writeFile(join(repoRoot, 'index.js'), 'export const migrate = () => {};\n'),
    writeFile(join(repoRoot, 'README.md'), 'readme\n'),
    writeFile(join(repoRoot, 'LICENSE'), 'license\n'),
  ]);

  await stageRootPackage({ repoRoot, stageRoot });

  assert.equal(await readFile(join(repoRoot, 'package.json'), 'utf8'), tracked);
  const staged = JSON.parse(await readFile(join(stageRoot, 'package.json'), 'utf8'));
  assert.deepEqual(Object.values(staged.optionalDependencies), ['1.2.3', '1.2.3']);
});

test('provenance rejects altered tarballs, commits, platforms, and package identities', async (t) => {
  const artifactRoot = await mkdtemp(join(tmpdir(), 'tw-migrate-provenance-test-'));
  t.after(() => rm(artifactRoot, { recursive: true, force: true }));
  await Promise.all([
    writeFile(join(artifactRoot, 'root.tgz'), 'root'),
    writeFile(join(artifactRoot, 'native.tgz'), 'native'),
    writeFile(join(artifactRoot, currentTarget().addon), 'addon'),
  ]);
  const target = currentTarget();
  const provenance = {
    commit: '0123456789abcdef0123456789abcdef01234567',
    platform: target.platform,
    packages: {
      root: { name: 'tw-migrate', version: '1.2.3', tarball: 'root.tgz', sha256: '4813494d137e1631bba301d5acab6e7bb7aa74ce1185d456565ef51d737677b2' },
      native: { name: target.packageName, version: '1.2.3', tarball: 'native.tgz', sha256: 'bef32d2c315a289576f2a6828d27edb16bb316a4d85c271f2d794045f3ea668d' },
    },
    addon: { file: target.addon, sha256: '613c3abf0f077f31505d3c8cc0fed9a94a49cf025af3e604c4d38259c1cdf4c7' },
  };
  await assert.doesNotReject(validateProvenance(provenance, { artifactRoot, expectedCommit: provenance.commit }));

  for (const invalid of [
    { ...provenance, commit: 'f'.repeat(40) },
    { ...provenance, platform: 'wrong-platform' },
    { ...provenance, packages: { ...provenance.packages, native: { ...provenance.packages.native, name: 'tw-migrate-wrong' } } },
  ]) {
    await assert.rejects(validateProvenance(invalid, { artifactRoot, expectedCommit: provenance.commit }));
  }
  await writeFile(join(artifactRoot, 'native.tgz'), 'altered');
  await assert.rejects(validateProvenance(provenance, { artifactRoot, expectedCommit: provenance.commit }), /digest/);
});

test('installed layout rejects checkout, symlink, wrong platform, and unexpected package paths', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'tw-migrate-layout-test-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const checkout = join(root, 'checkout');
  const driverRoot = join(root, 'driver');
  const target = currentTarget();
  const rootPackage = join(driverRoot, 'node_modules', 'tw-migrate');
  const nativePackage = join(driverRoot, 'node_modules', target.packageName);
  await Promise.all([mkdir(checkout), mkdir(rootPackage, { recursive: true }), mkdir(nativePackage, { recursive: true })]);
  await Promise.all([
    writeFile(join(rootPackage, 'package.json'), JSON.stringify({ name: 'tw-migrate', version: '1.2.3' })),
    writeFile(join(nativePackage, 'package.json'), JSON.stringify({ name: target.packageName, version: '1.2.3' })),
    writeFile(join(nativePackage, target.addon), 'addon'),
  ]);
  const expected = { version: '1.2.3', platform: target.platform, addonSha256: '613c3abf0f077f31505d3c8cc0fed9a94a49cf025af3e604c4d38259c1cdf4c7' };
  await assert.doesNotReject(assertInstalledLayout({ driverRoot, checkoutRoot: checkout, expected }));
  await assert.rejects(assertInstalledLayout({ driverRoot, checkoutRoot: root, expected }), /checkout/);
  await assert.rejects(assertInstalledLayout({ driverRoot, checkoutRoot: checkout, expected: { ...expected, platform: 'darwin-x64' } }));

  await rm(nativePackage, { recursive: true });
  await mkdir(join(checkout, target.packageName));
  await writeFile(join(checkout, target.packageName, 'package.json'), JSON.stringify({ name: target.packageName, version: '1.2.3' }));
  await writeFile(join(checkout, target.packageName, target.addon), 'addon');
  await mkdir(join(driverRoot, 'node_modules'), { recursive: true });
  await import('node:fs/promises').then(({ symlink }) => symlink(join(checkout, target.packageName), nativePackage));
  await assert.rejects(assertInstalledLayout({ driverRoot, checkoutRoot: checkout, expected }), /checkout|node_modules|workspace/);
});

test('sealed registry config proxies dependencies but never product packages or mutations', () => {
  const config = registryConfig({ storage: '/tmp/storage', allowPublish: false });
  assert.match(config, /tw-migrate-\*/);
  assert.match(config, /proxy: false/);
  assert.match(config, /publish: nobody/);
  assert.match(config, /'\*\*':[\s\S]*proxy: npmjs/);
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
