import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { closeSync, openSync } from 'node:fs';
import { appendFile, cp, lstat, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { assertInstalledLayout, publishPackages, stagePackages, validateProvenance } from './packages.js';
import { startRegistry } from './registry.js';
import { assertOracle, captureAll, maxCaptureAttempts } from './oracle.js';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const caches = ['.next', 'dist', 'node_modules/.vite'];
export const lifecycleTimeoutMs = 30 * 60_000;

function inside(path, root) {
  const rel = relative(root, path);
  return rel === '' || (!rel.startsWith('..') && !isAbsolute(rel));
}

export async function artifactAllowlist(root, entries, maxBytes = 100 * 1024 * 1024) {
  root = resolve(root);
  const canonicalRoot = await realpath(root);
  const paths = [];
  let bytes = 0;
  for (const entry of entries) {
    const path = resolve(root, entry);
    if (!inside(path, root)) throw new Error(`artifact path escapes root: ${entry}`);
    const stat = await lstat(path);
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`artifact is not a regular file: ${entry}`);
    if (!inside(await realpath(path), canonicalRoot)) throw new Error(`artifact path escapes root through a symlink: ${entry}`);
    bytes += stat.size;
    if (bytes > maxBytes) throw new Error(`artifact allowlist exceeds ${maxBytes} bytes`);
    paths.push(path);
  }
  return paths;
}

export function assertMigrationContract({ first, expectedFirst, actualSource, expectedSource, second, treeBeforeSecond, treeAfterSecond }) {
  assert.deepEqual(first, expectedFirst, 'exact first MigrationReport');
  assert.equal(actualSource, expectedSource, 'exact migration-owned source');
  assert.deepEqual(second.changedFiles, [], 'second migration changedFiles');
  assert.equal(second.diff, '', 'second migration diff');
  assert.deepEqual(treeAfterSecond, treeBeforeSecond, 'source-scoped tree after second migration');
}

async function availablePort() {
  return new Promise((resolvePort, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close((error) => error ? reject(error) : resolvePort(port));
    });
  });
}

async function terminateTree(child) {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise((resolveExit) => child.once('exit', resolveExit));
  if (process.platform === 'win32') {
    spawnSync('taskkill.exe', ['/pid', String(child.pid), '/t', '/f'], { windowsHide: true });
  } else {
    try { process.kill(-child.pid, 'SIGTERM'); } catch {}
  }
  const stopped = await Promise.race([
    exited.then(() => true),
    new Promise((resolveWait) => setTimeout(() => resolveWait(false), 3_000)),
  ]);
  if (!stopped && process.platform !== 'win32') {
    try { process.kill(-child.pid, 'SIGKILL'); } catch {}
  }
  if (!stopped) {
    const forced = await Promise.race([
      exited.then(() => true),
      new Promise((resolveWait) => setTimeout(() => resolveWait(false), 3_000)),
    ]);
    if (!forced) throw new Error(`child process ${child.pid} did not exit`);
  }
}

async function startServer(project, cwd, artifactRoot, phase) {
  const port = await availablePort();
  const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
  const args = project.runtime === 'next'
    ? ['run', 'dev', '--', '--hostname', '127.0.0.1', '--port', String(port)]
    : ['run', 'dev', '--', '--host', '127.0.0.1', '--port', String(port), '--strictPort'];
  const logPath = join(artifactRoot, `${phase}-server.log`);
  const log = openSync(logPath, 'a');
  const child = spawn(npm, args, {
    cwd,
    detached: process.platform !== 'win32',
    windowsHide: true,
    stdio: ['ignore', log, log],
  });
  const url = `http://127.0.0.1:${port}`;
  const deadline = Date.now() + 60_000;
  try {
    while (Date.now() < deadline) {
      if (child.exitCode !== null) throw new Error(`${phase} server exited with ${child.exitCode}`);
      try {
        const response = await fetch(url, { signal: AbortSignal.timeout(1_000) });
        if (response.ok) return {
          url,
          async stop() { await terminateTree(child); closeSync(log); },
        };
      } catch {}
      await new Promise((resolveWait) => setTimeout(resolveWait, 200));
    }
    throw new Error(`${phase} server readiness timed out`);
  } catch (error) {
    await terminateTree(child);
    closeSync(log);
    throw error;
  }
}

async function run(command, args, { cwd, logPath, timeoutMs = 180_000 }) {
  const log = openSync(logPath, 'a');
  const child = spawn(command, args, {
    cwd,
    detached: process.platform !== 'win32',
    windowsHide: true,
    stdio: ['ignore', log, log],
  });
  const timer = setTimeout(() => terminateTree(child), timeoutMs);
  const result = await new Promise((resolveRun, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolveRun({ code, signal }));
  }).finally(() => { clearTimeout(timer); closeSync(log); });
  if (result.code !== 0) throw new Error(`${command} ${args.join(' ')} failed (${result.signal ?? result.code}); see ${logPath}`);
}

async function hashMigrationPaths(root, paths) {
  const result = {};
  for (const relativePath of [...paths].sort()) {
    const path = resolve(root, relativePath);
    if (!inside(path, root)) throw new Error(`migration-owned path escapes project: ${relativePath}`);
    const stat = await lstat(path).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (stat === null) {
      result[relativePath] = null;
    } else {
      if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`migration-owned path is not a regular file: ${relativePath}`);
      result[relativePath] = createHash('sha256').update(await readFile(path)).digest('hex');
    }
  }
  return result;
}

async function prepareDriver(project, runRoot, packageArtifactRoot, artifactRoot) {
  await mkdir(packageArtifactRoot, { recursive: true });
  const provenancePath = join(packageArtifactRoot, 'provenance.json');
  let provenance;
  try {
    provenance = JSON.parse(await readFile(provenancePath, 'utf8'));
    const git = spawnSync('git', ['rev-parse', 'HEAD'], { cwd: repoRoot, encoding: 'utf8' });
    if (git.status !== 0) throw new Error('could not verify package provenance commit');
    await validateProvenance(provenance, { artifactRoot: packageArtifactRoot, expectedCommit: git.stdout.trim() });
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    provenance = await stagePackages({ repoRoot, artifactRoot: packageArtifactRoot });
  }

  const registryRoot = join(runRoot, 'registry');
  const bootstrapRegistry = await startRegistry({ root: registryRoot, artifactRoot, allowPublish: true });
  try {
    await publishPackages(provenance, packageArtifactRoot, bootstrapRegistry.url);
  } finally {
    await bootstrapRegistry.stop();
  }
  const sealedRegistry = await startRegistry({ root: registryRoot, artifactRoot, allowPublish: false });
  const fixture = join(repoRoot, 'ecosystem-ci', 'fixtures', 'controlled', project.runtime, project.style);
  const driverRoot = join(runRoot, 'driver');
  try {
    await cp(fixture, driverRoot, { recursive: true });
    const manifestPath = join(driverRoot, 'package.json');
    const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
    manifest.dependencies['tw-migrate'] = provenance.packages.root.version;
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
    await run(process.platform === 'win32' ? 'npm.cmd' : 'npm', [
      'install', '--registry', sealedRegistry.url, '--ignore-scripts', '--no-audit', '--no-fund', '--fetch-retries=0',
    ], { cwd: driverRoot, logPath: join(artifactRoot, 'install.log') });
  } finally {
    await sealedRegistry.stop();
  }
  const installed = await assertInstalledLayout({
    driverRoot,
    checkoutRoot: repoRoot,
    expected: { version: provenance.packages.root.version, platform: provenance.platform, addonSha256: provenance.addon.sha256 },
  });
  return { driverRoot, installed };
}

async function readMaybe(path) {
  return readFile(path, 'utf8').catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
}

export function captureAttemptArtifactNames(phase, probe, attempt) {
  return [`${phase}-${probe}-attempt-${attempt}-browser.json`, `${phase}-${probe}-attempt-${attempt}.png`];
}

function captureArtifactNames(project, phase) {
  return [
    `${phase}-computed.json`,
    `${phase}-server.log`,
    ...Object.keys(project.probes).flatMap((probe) =>
      Array.from({ length: maxCaptureAttempts }, (_, index) => captureAttemptArtifactNames(phase, probe, index + 1)).flat()),
  ];
}

async function existingArtifactNames(artifactRoot, names) {
  const existing = [];
  for (const name of names) {
    if ((await lstat(join(artifactRoot, name)).catch(() => null))?.isFile()) existing.push(name);
  }
  return existing;
}

function caseArtifactNames(project) {
  return ['phase-ledger.json', 'failure.log', 'install.log', 'registry-bootstrap.log', 'registry-install.log',
    'first-report.json', 'second-report.json', 'source.diff',
    ...['baseline', 'withheld', 'utilities-only', 'post'].flatMap((phase) => captureArtifactNames(project, phase))];
}

export async function prepareCaseUpload(project, artifactRoot, uploadRoot) {
  const allowed = new Set(caseArtifactNames(project));
  const ledger = JSON.parse(await readFile(join(artifactRoot, 'phase-ledger.json'), 'utf8'));
  const declared = new Set(['phase-ledger.json', ...(ledger.failureFiles ?? [])]);
  for (const phase of ledger.phases) for (const file of phase.files) declared.add(file);
  if (ledger.failure) declared.add('failure.log');
  const existing = [];
  for (const name of declared) {
    if (!allowed.has(name)) throw new Error(`phase ledger declared forbidden artifact: ${name}`);
    if ((await lstat(join(artifactRoot, name)).catch(() => null))?.isFile()) existing.push(name);
  }
  await artifactAllowlist(artifactRoot, existing);
  await rm(uploadRoot, { recursive: true, force: true });
  for (const name of existing) {
    const destination = join(uploadRoot, name);
    await mkdir(dirname(destination), { recursive: true });
    await cp(join(artifactRoot, name), destination);
  }
  return existing;
}

export function temporaryLifecyclePaths(projectId, temporaryRoot) {
  return {
    artifactRoot: join(temporaryRoot, 'artifacts', projectId),
    packageArtifactRoot: join(temporaryRoot, 'packages'),
  };
}

export async function runLifecycle({
  browser,
  project,
  artifactRoot,
  packageArtifactRoot = process.env.ECOSYSTEM_PACKAGE_ARTIFACT_ROOT,
}) {
  let temporaryRoot;
  if (!artifactRoot) {
    temporaryRoot = await mkdtemp(join(tmpdir(), `tw-migrate-${project.id}-artifacts-`));
    ({ artifactRoot, packageArtifactRoot } = temporaryLifecyclePaths(project.id, temporaryRoot));
  }
  artifactRoot = resolve(artifactRoot);
  packageArtifactRoot = packageArtifactRoot ? resolve(packageArtifactRoot) : artifactRoot;
  await rm(artifactRoot, { recursive: true, force: true });
  await mkdir(artifactRoot, { recursive: true });
  const ledger = { case: project.id, phases: [] };
  const ledgerPath = join(artifactRoot, 'phase-ledger.json');
  const mark = async (phase, files = []) => {
    ledger.phases.push({ phase, files });
    await writeFile(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`);
  };
  await mark('initialized');
  const runRoot = await mkdtemp(join(tmpdir(), `tw-migrate-${project.id}-`));
  let server;
  let succeeded = false;
  try {
    const { driverRoot, installed } = await prepareDriver(project, runRoot, packageArtifactRoot, artifactRoot);
    await mark('installed', ['install.log', 'registry-bootstrap.log', 'registry-install.log']);
    const diagnostic = (phase, name, attempt) => {
      const [browserJson, screenshot] = captureAttemptArtifactNames(phase, name, attempt);
      return {
        screenshot: join(artifactRoot, screenshot),
        writeDiagnostics: (value) => writeFile(join(artifactRoot, browserJson), `${JSON.stringify(value, null, 2)}\n`),
      };
    };

    await mark('baseline-started');
    server = await startServer(project, driverRoot, artifactRoot, 'baseline');
    const baseline = await captureAll(browser, server.url, project.probes, (name, attempt) => diagnostic('baseline', name, attempt));
    await writeFile(join(artifactRoot, 'baseline-computed.json'), `${JSON.stringify(baseline, null, 2)}\n`);
    await mark('baseline', await existingArtifactNames(artifactRoot, captureArtifactNames(project, 'baseline')));
    await server.stop(); server = undefined;

    const sourcePath = join(driverRoot, project.source.path);
    const authored = await readFile(sourcePath, 'utf8');
    assert.ok(authored.includes(project.source.before), 'causal witness source token');
    const expected = JSON.parse(await readFile(join(driverRoot, 'expected.json'), 'utf8'));
    assert.ok(expected.first.candidates.includes(project.source.after), 'causal witness candidate token');
    await writeFile(sourcePath, '');
    await mark('causal-witness-started');
    server = await startServer(project, driverRoot, artifactRoot, 'withheld');
    const withheld = await captureAll(browser, server.url, project.probes, (name, attempt) => diagnostic('withheld', name, attempt));
    await writeFile(join(artifactRoot, 'withheld-computed.json'), `${JSON.stringify(withheld, null, 2)}\n`);
    await server.stop(); server = undefined;
    await writeFile(sourcePath, authored);
    await mark('causal-witness', await existingArtifactNames(artifactRoot, captureArtifactNames(project, 'withheld')));

    await mark('migration-started');
    const module = await import(`${pathToFileURL(join(installed.root, 'index.js')).href}?case=${Date.now()}`);
    const first = await module.migrate({ cwd: driverRoot, styleFile: project.source.path, write: true });
    await writeFile(join(artifactRoot, 'first-report.json'), `${JSON.stringify(first, null, 2)}\n`);
    await writeFile(join(artifactRoot, 'source.diff'), first.diff);
    const actualSource = await readMaybe(sourcePath);
    const treeBeforeSecond = await hashMigrationPaths(driverRoot, first.changedFiles);
    const second = await module.migrate({ cwd: driverRoot, styleFile: project.source.path, write: true });
    const treeAfterSecond = await hashMigrationPaths(driverRoot, first.changedFiles);
    await writeFile(join(artifactRoot, 'second-report.json'), `${JSON.stringify(second, null, 2)}\n`);
    await mark('migration-output', ['first-report.json', 'second-report.json', 'source.diff']);
    assertMigrationContract({ first, expectedFirst: expected.first, actualSource, expectedSource: expected.source, second, treeBeforeSecond, treeAfterSecond });
    await mark('migration');

    await writeFile(sourcePath, '');
    await mark('utilities-only-started');
    try {
      await Promise.all(caches.map((path) => rm(join(driverRoot, path), { recursive: true, force: true })));
      server = await startServer(project, driverRoot, artifactRoot, 'utilities-only');
      const utilitiesOnly = await captureAll(browser, server.url, project.probes, (name, attempt) => diagnostic('utilities-only', name, attempt));
      await writeFile(join(artifactRoot, 'utilities-only-computed.json'), `${JSON.stringify(utilitiesOnly, null, 2)}\n`);
      await mark('utilities-only-captured', await existingArtifactNames(artifactRoot, captureArtifactNames(project, 'utilities-only')));
      assert.deepEqual(
        Object.fromEntries(Object.entries(utilitiesOnly).map(([name, value]) => [name, value.elements])),
        Object.fromEntries(Object.entries(baseline).map(([name, value]) => [name, value.elements])),
        'utilities-only computed capture exactly equals baseline',
      );
    } finally {
      await server?.stop(); server = undefined;
      await writeFile(sourcePath, actualSource);
    }
    await mark('utilities-only');

    await mark('post-started');
    await Promise.all(caches.map((path) => rm(join(driverRoot, path), { recursive: true, force: true })));
    server = await startServer(project, driverRoot, artifactRoot, 'post');
    const post = await captureAll(browser, server.url, project.probes, (name, attempt) => diagnostic('post', name, attempt));
    await writeFile(join(artifactRoot, 'post-computed.json'), `${JSON.stringify(post, null, 2)}\n`);
    await mark('post-captured', await existingArtifactNames(artifactRoot, captureArtifactNames(project, 'post')));
    assertOracle({ baseline, post, withheld, candidateTokens: expected.first.candidates });
    await mark('complete');
    succeeded = true;
    return { baseline, first, second, post, ledger };
  } catch (error) {
    await appendFile(join(artifactRoot, 'failure.log'), `${error.stack ?? error}\n`).catch(() => {});
    ledger.failure = error.message;
    ledger.failureFiles = [];
    for (const name of caseArtifactNames(project)) {
      if ((await lstat(join(artifactRoot, name)).catch(() => null))?.isFile()) ledger.failureFiles.push(name);
    }
    await writeFile(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`).catch(() => {});
    throw error;
  } finally {
    await server?.stop().catch(() => {});
    await rm(runRoot, { recursive: true, force: true });
    if (succeeded && temporaryRoot) await rm(temporaryRoot, { recursive: true, force: true });
  }
}
