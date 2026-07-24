import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { EventEmitter } from 'node:events';
import { mkdtemp, mkdir, readFile, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  assertInstalledLayout,
  currentTarget,
  packageUploadRoot,
  preparePackageUpload,
  publisherToken,
  stageRootPackage,
  validateProvenance,
} from '../ecosystem-ci/packages.js';
import { registryConfig } from '../ecosystem-ci/registry.js';
import { assertOracle, captureProbe, normalizeStyleEntries, retryCapture, withTimeout } from '../ecosystem-ci/oracle.js';
import {
  artifactAllowlist,
  assertExpectedChangedFiles,
  assertMigrationContract,
  captureAttemptArtifactNames,
  snapshotMigrationSources,
  teardownLifecycleServer,
  temporaryLifecyclePaths,
  waitForChild,
} from '../ecosystem-ci/lifecycle.js';
import { ecosystemMatrix, loadManifest, runHarness, validateManifest } from '../ecosystem-ci/run.js';

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
    identity: ['card'],
    ...overrides,
  };
}

function controlled(overrides = {}) {
  return {
    id: 'react-vite-css',
    kind: 'controlled',
    runtime: 'react-vite',
    style: 'css',
    source: { path: 'src/App.module.css', before: '.card', after: 'p-[13px]' },
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
  for (const field of ['route', 'viewport', 'readiness', 'selector', 'cardinality', 'identity']) {
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

test('package stage CLI creates the exact upload tree consumed by the workflow', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'tw-migrate-package-upload-test-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const artifactRoot = join(root, 'package-artifacts');
  const target = currentTarget();
  const provenance = {
    packages: {
      root: { tarball: 'tarballs/root.tgz' },
      native: { tarball: 'tarballs/native.tgz' },
    },
    addon: { file: `staging/native/${target.addon}` },
  };
  await Promise.all([
    mkdir(join(artifactRoot, 'tarballs'), { recursive: true }),
    mkdir(join(artifactRoot, 'staging', 'native'), { recursive: true }),
  ]);
  await Promise.all([
    writeFile(join(artifactRoot, 'provenance.json'), '{}\n'),
    writeFile(join(artifactRoot, provenance.packages.root.tarball), 'root'),
    writeFile(join(artifactRoot, provenance.packages.native.tarball), 'native'),
    writeFile(join(artifactRoot, provenance.addon.file), 'addon'),
  ]);

  const uploadRoot = packageUploadRoot(artifactRoot);
  await preparePackageUpload(provenance, artifactRoot, uploadRoot);

  assert.equal(uploadRoot, `${artifactRoot}-upload`);
  assert.deepEqual((await readdir(uploadRoot)).sort(), ['provenance.json', 'staging', 'tarballs']);
  assert.deepEqual((await readdir(join(uploadRoot, 'tarballs'))).sort(), ['native.tgz', 'root.tgz']);
  assert.deepEqual(await readdir(join(uploadRoot, 'staging')), ['native']);
  assert.deepEqual(await readdir(join(uploadRoot, 'staging', 'native')), [target.addon]);
  const workflow = await readFile(new URL('../.github/workflows/ecosystem.yml', import.meta.url), 'utf8');
  assert.match(workflow, /packages\.js stage --artifact-root ecosystem-ci\/package-artifacts/);
  assert.match(workflow, /path: ecosystem-ci\/package-artifacts-upload\//);
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
  await symlink(join(checkout, target.packageName), nativePackage);
  await assert.rejects(assertInstalledLayout({ driverRoot, checkoutRoot: checkout, expected }), /checkout|node_modules|workspace/);
});

test('publisher credential response body is bounded by the registry startup timeout', async (t) => {
  let requested = false;
  const server = createServer((_request, response) => {
    requested = true;
    response.writeHead(200, { 'content-type': 'application/json' });
    response.write('{"token":"');
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));

  const { port } = server.address();
  await assert.rejects(publisherToken(`http://127.0.0.1:${port}`, 250), { name: 'TimeoutError' });
  assert.equal(requested, true);
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
  assert.deepEqual(calls, [['run', '--config', 'ecosystem-ci/vitest.config.js', '--project', 'react-vite-css']]);
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
  assert.deepEqual(calls, [['run', '--config', 'ecosystem-ci/vitest.config.js']]);
});

test('the browser oracle sorts every standard computed property and excludes only custom properties', () => {
  assert.deepEqual(normalizeStyleEntries([
    ['z-index', 'auto'],
    ['--fixture-token', 'secret'],
    ['-webkit-font-smoothing', 'auto'],
    ['color', 'rgb(1, 2, 3)'],
  ]), {
    '-webkit-font-smoothing': 'auto',
    color: 'rgb(1, 2, 3)',
    'z-index': 'auto',
  });
});

test('the causal witness requires a standard computed-property change for every probe', () => {
  const capture = (color, token) => ({ elements: [{ identity: 'card', styles: { color, '--fixture-token': token } }] });
  const baseline = { base: capture('red', 'one'), hover: capture('red', 'one') };
  assert.throws(() => assertOracle({
    baseline,
    post: structuredClone(baseline),
    withheld: { base: capture('black', 'two'), hover: capture('red', 'two') },
    candidateTokens: ['text-red'],
  }), /standard computed property for probe "hover"/);
});

test('capture retry permits initial plus three retries and creates fresh attempts', async () => {
  const attempts = [];
  const result = await retryCapture(async (attempt) => {
    attempts.push(attempt);
    if (attempt < 4) throw new Error('navigation failed');
    return 'captured';
  });
  assert.equal(result, 'captured');
  assert.deepEqual(attempts, [1, 2, 3, 4]);
  assert.deepEqual(captureAttemptArtifactNames('baseline', 'base', 3), [
    'baseline-base-attempt-3-browser.json',
    'baseline-base-attempt-3.png',
  ]);
  await assert.rejects(retryCapture(async () => { throw new Error('still broken'); }), /still broken/);
});

test('capture attempt timeout rejects a stalled operation', async () => {
  await assert.rejects(withTimeout(() => new Promise(() => {}), 10), /timed out after 10ms/);
});

test('page-creation failures keep diagnostics for all four attempts', async () => {
  const diagnostics = [];
  await assert.rejects(
    captureProbe(
      { newPage: async () => { throw new Error('browser unavailable'); } },
      'http://127.0.0.1/',
      probe(),
      (attempt) => ({ writeDiagnostics: (value) => diagnostics.push({ attempt, ...value }) }),
    ),
    /browser unavailable/,
  );
  assert.deepEqual(diagnostics.map(({ attempt }) => attempt), [1, 2, 3, 4]);
  assert.ok(diagnostics.every(({ error }) => error.includes('browser unavailable')));
});

test('contributor lifecycle defaults keep case and package artifacts under one OS temporary root', () => {
  const root = join(tmpdir(), 'tw-migrate-test-root');
  assert.deepEqual(temporaryLifecyclePaths('react-vite-css', root), {
    artifactRoot: join(root, 'artifacts', 'react-vite-css'),
    packageArtifactRoot: join(root, 'packages'),
  });
});

test('migration contract checks the exact report/source and no-op second run without retries', () => {
  const first = { changedFiles: ['src/a.css'], diff: 'diff', candidates: ['p-[13px]'], warnings: [] };
  assert.doesNotThrow(() => assertMigrationContract({
    first,
    expectedFirst: first,
    actualSource: 'after\n',
    expectedSource: 'after\n',
    second: { changedFiles: [], diff: '' },
    treeBeforeSecond: { 'src/a.css': 'abc' },
    treeAfterSecond: { 'src/a.css': 'abc' },
  }));
  assert.throws(() => assertMigrationContract({
    first,
    expectedFirst: first,
    actualSource: 'wrong',
    expectedSource: 'after\n',
    second: { changedFiles: [], diff: '' },
    treeBeforeSecond: {},
    treeAfterSecond: {},
  }), /source/);
});

test('source-wide idempotency catches an unreported extra source mutation', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'tw-migrate-source-tree-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(join(root, 'src'));
  await Promise.all([
    writeFile(join(root, 'src', 'reported.css'), 'reported\n'),
    writeFile(join(root, 'src', 'unreported.tsx'), 'before\n'),
  ]);
  const before = await snapshotMigrationSources(root);
  await writeFile(join(root, 'src', 'unreported.tsx'), 'after\n');
  const after = await snapshotMigrationSources(root);
  assert.throws(() => assertMigrationContract({
    first: { changedFiles: ['src/reported.css'] },
    expectedFirst: { changedFiles: ['src/reported.css'] },
    actualSource: 'reported\n',
    expectedSource: 'reported\n',
    second: { changedFiles: [], diff: '' },
    treeBeforeSecond: before,
    treeAfterSecond: after,
  }), /source-scoped tree/);
});

test('source snapshots exclude generated trees and reject non-regular paths', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'tw-migrate-source-safety-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(join(root, 'node_modules'));
  await writeFile(join(root, 'node_modules', 'generated.js'), 'ignored\n');
  assert.deepEqual(await snapshotMigrationSources(root), {});
  await symlink(join(root, 'node_modules', 'generated.js'), join(root, 'linked.js'));
  await assert.rejects(snapshotMigrationSources(root), /not regular/);
});

test('exact changed-file validation requires complete paths and bytes', () => {
  const changedFiles = ['src/App.jsx', 'src/App.css'];
  const files = { 'src/App.jsx': 'consumer\n', 'src/App.css': 'style\n' };
  assert.doesNotThrow(() => assertExpectedChangedFiles(changedFiles, files, files));
  assert.throws(() => assertExpectedChangedFiles(changedFiles, { 'src/App.css': 'style\n' }, files), /cover changedFiles/);
  assert.throws(() => assertExpectedChangedFiles(changedFiles, files, { ...files, 'src/App.jsx': 'wrong\n' }), /exact post-migration bytes/);
});

test('controlled expectations cover every reported changed file with exact bytes', async () => {
  for (const runtime of ['react-vite', 'next', 'vite-html']) {
    const expected = JSON.parse(await readFile(new URL(`../ecosystem-ci/fixtures/controlled/${runtime}/css/expected.json`, import.meta.url)));
    assert.deepEqual(Object.keys(expected.changedFiles).sort(), [...expected.first.changedFiles].sort());
    assert.ok(Object.values(expected.changedFiles).every((contents) => typeof contents === 'string'));
  }
});

test('command timeout awaits and bounds teardown failures', async () => {
  await assert.rejects(waitForChild(new EventEmitter(), {
    timeoutMs: 1,
    terminate: async () => { throw new Error('kill failed'); },
  }), /timed out.*teardown failed: kill failed/);
  await assert.rejects(waitForChild(new EventEmitter(), {
    timeoutMs: 1,
    teardownTimeoutMs: 5,
    terminate: () => new Promise(() => {}),
  }), /teardown timed out after 5ms/);
});

test('final server teardown records and propagates only when lifecycle otherwise succeeded', async () => {
  const failure = new Error('stop failed');
  const recorded = [];
  const server = { stop: async () => { throw failure; } };
  await assert.rejects(teardownLifecycleServer(server, undefined, async (error) => recorded.push(error)), failure);
  assert.deepEqual(recorded, [failure]);
  await assert.doesNotReject(teardownLifecycleServer(server, new Error('primary'), async () => assert.fail('must preserve primary')));
});

test('workflow artifact allowlist rejects traversal, symlinks, directories, and undeclared files', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'tw-migrate-artifacts-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await writeFile(join(root, 'phase-ledger.json'), '{}');
  assert.deepEqual(await artifactAllowlist(root, ['phase-ledger.json']), [join(root, 'phase-ledger.json')]);
  await assert.rejects(artifactAllowlist(root, ['../outside']), /escapes/);
  await mkdir(join(root, 'directory'));
  await assert.rejects(artifactAllowlist(root, ['directory']), /regular file/);
  await symlink(join(root, 'phase-ledger.json'), join(root, 'link'));
  await assert.rejects(artifactAllowlist(root, ['link']), /regular file|symlink/);
});

test('every manifest probe declares exact stable target identities', async () => {
  const loaded = await loadManifest();
  for (const project of loaded.projects) {
    for (const probe of Object.values(project.probes)) {
      assert.equal(probe.identity.length, probe.cardinality);
      assert.ok(probe.identity.every((value) => typeof value === 'string' && value.length > 0));
    }
  }
});

test('controlled manifest expands to the exact three-OS by three-case workflow matrix', async () => {
  const matrix = ecosystemMatrix(await loadManifest());
  assert.equal(matrix.length, 9);
  assert.deepEqual(new Set(matrix.map(({ os }) => os)), new Set(['linux', 'macos', 'windows']));
  assert.deepEqual(new Set(matrix.map((entry) => entry.case)), new Set(['react-vite-css', 'next-css', 'vite-html-css']));
  assert.ok(matrix.every(({ runner }) => ['ubuntu-latest', 'macos-latest', 'windows-latest'].includes(runner)));
});

test('case jobs run after non-cancelled partial package failure while preserving label gating', async () => {
  const workflow = await readFile(new URL('../.github/workflows/ecosystem.yml', import.meta.url), 'utf8');
  assert.match(workflow, /^  case:\n    needs: package\n    if: \$\{\{ !cancelled\(\) && \(github\.event_name != 'pull_request' \|\| github\.event\.label\.name == 'ecosystem'\) \}\}$/m);
});

test('fixture integration dependencies use the required exact pins', async () => {
  const expected = {
    'react-vite': { '@tailwindcss/vite': '4.3.3', tailwindcss: '4.3.3', vite: '8.1.5', react: '19.2.8', 'react-dom': '19.2.8' },
    next: { '@tailwindcss/postcss': '4.3.3', tailwindcss: '4.3.3', next: '15.5.21', react: '19.2.8', 'react-dom': '19.2.8' },
    'vite-html': { '@tailwindcss/vite': '4.3.3', tailwindcss: '4.3.3', vite: '8.1.5' },
  };
  for (const [runtime, pins] of Object.entries(expected)) {
    const fixture = JSON.parse(await readFile(new URL(`../ecosystem-ci/fixtures/controlled/${runtime}/css/package.json`, import.meta.url)));
    for (const [name, version] of Object.entries(pins)) assert.equal(fixture.dependencies[name], version);
  }
});

test('the no-argument CLI stays browser-free and returns usage', () => {
  assert.throws(
    () => execFileSync(process.execPath, ['ecosystem-ci/run.js'], { encoding: 'utf8', stdio: 'pipe' }),
    (error) => error.status === 1 && /Usage:/.test(error.stderr),
  );
});
