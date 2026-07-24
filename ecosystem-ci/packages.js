#!/usr/bin/env node

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { closeSync, openSync } from 'node:fs';
import {
  appendFile,
  cp,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  realpath,
  rm,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { startRegistry } from './registry.js';

const targets = {
  'darwin-arm64': 'darwin-arm64',
  'darwin-x64': 'darwin-x64',
  'linux-arm64': 'linux-arm64-gnu',
  'linux-x64': 'linux-x64-gnu',
  'win32-x64': 'win32-x64-msvc',
};

function targetFor(platform, arch) {
  const target = targets[`${platform}-${arch}`];
  if (!target) throw new Error(`Unsupported platform: ${platform}-${arch}`);
  return {
    platform: target,
    packageName: `tw-migrate-${target}`,
    addon: `tw-migrate.${target}.node`,
  };
}

export function currentTarget() {
  return targetFor(process.platform, process.arch);
}

function inside(path, parent) {
  const rel = relative(parent, path);
  return rel === '' || (!rel.startsWith('..') && !isAbsolute(rel));
}

async function sha256(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex');
}

async function run(command, args, { cwd, logPath, timeoutMs = 120_000 }) {
  await mkdir(dirname(logPath), { recursive: true });
  const log = openSync(logPath, 'a');
  const child = spawn(command, args, { cwd, stdio: ['ignore', log, log], windowsHide: true });
  const timer = setTimeout(() => child.kill('SIGKILL'), timeoutMs);
  const result = await new Promise((resolveRun, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolveRun({ code, signal }));
  }).finally(() => {
    clearTimeout(timer);
    closeSync(log);
  });
  if (result.code !== 0) {
    throw new Error(`${command} ${args.join(' ')} failed (${result.signal ?? result.code}); log: ${logPath}`);
  }
}

async function npmPack(packageDir, destination, logPath) {
  const before = new Set((await readdir(destination)).filter((file) => file.endsWith('.tgz')));
  await run(process.platform === 'win32' ? 'npm.cmd' : 'npm', ['pack', '--pack-destination', destination], {
    cwd: packageDir,
    logPath,
  });
  const created = (await readdir(destination)).filter((file) => file.endsWith('.tgz') && !before.has(file));
  if (created.length !== 1) throw new Error(`npm pack in ${packageDir} created ${created.length} tarballs, expected one`);
  return join(destination, created[0]);
}

export async function stageRootPackage({ repoRoot, stageRoot }) {
  const manifestPath = join(repoRoot, 'package.json');
  const tracked = await readFile(manifestPath);
  const manifest = JSON.parse(tracked);
  if (typeof manifest.version !== 'string') throw new Error('tracked package.json has no string version');
  if (!manifest.optionalDependencies || Array.isArray(manifest.optionalDependencies)) {
    throw new Error('tracked package.json has no optionalDependencies object');
  }
  if (!Array.isArray(manifest.files) || manifest.files.some((file) => typeof file !== 'string')) {
    throw new Error('tracked package.json has no valid files array');
  }
  for (const dependency of Object.keys(manifest.optionalDependencies)) {
    manifest.optionalDependencies[dependency] = manifest.version;
  }

  await rm(stageRoot, { recursive: true, force: true });
  await mkdir(stageRoot, { recursive: true });
  for (const path of [...manifest.files, 'README.md', 'LICENSE']) {
    const source = join(repoRoot, path);
    try {
      await cp(source, join(stageRoot, path), { recursive: true });
    } catch (error) {
      throw new Error(`publishable package path is missing: ${source}`, { cause: error });
    }
  }
  await writeFile(join(stageRoot, 'package.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  if (!tracked.equals(await readFile(manifestPath))) throw new Error('package staging modified tracked package.json');
  return manifest;
}

export async function stagePackages({ repoRoot, artifactRoot }) {
  repoRoot = await realpath(repoRoot);
  artifactRoot = resolve(artifactRoot);
  const staging = join(artifactRoot, 'staging');
  const tarballs = join(artifactRoot, 'tarballs');
  const logPath = join(artifactRoot, 'package-setup.log');
  await rm(staging, { recursive: true, force: true });
  await rm(tarballs, { recursive: true, force: true });
  await Promise.all([mkdir(staging, { recursive: true }), mkdir(tarballs, { recursive: true })]);

  const target = currentTarget();
  const nativeStage = join(staging, 'native');
  const nativeSource = join(repoRoot, 'npm', target.platform);
  await cp(nativeSource, nativeStage, { recursive: true });
  const addonPath = join(nativeStage, target.addon);
  if (!(await lstat(addonPath).catch(() => null))?.isFile()) {
    throw new Error(`native release artifact is missing at ${join(nativeSource, target.addon)}; run \`pnpm build && pnpm artifacts\` first`);
  }

  const nativeManifest = JSON.parse(await readFile(join(nativeStage, 'package.json'), 'utf8'));
  if (nativeManifest.name !== target.packageName) throw new Error(`native package name must be ${target.packageName}`);
  const nativeTarball = await npmPack(nativeStage, tarballs, logPath);

  const rootStage = join(staging, 'root');
  const rootManifest = await stageRootPackage({ repoRoot, stageRoot: rootStage });
  if (rootManifest.name !== 'tw-migrate') throw new Error('root package name must be tw-migrate');
  if (nativeManifest.version !== rootManifest.version) throw new Error('root and native package versions differ');
  const rootTarball = await npmPack(rootStage, tarballs, logPath);
  const commitLog = join(artifactRoot, 'git.log');
  await run('git', ['rev-parse', 'HEAD'], { cwd: repoRoot, logPath: commitLog });
  const commit = (await readFile(commitLog, 'utf8')).trim().split(/\r?\n/).at(-1);

  const provenance = {
    commit,
    platform: target.platform,
    packages: {
      root: {
        name: rootManifest.name,
        version: rootManifest.version,
        tarball: relative(artifactRoot, rootTarball),
        sha256: await sha256(rootTarball),
      },
      native: {
        name: nativeManifest.name,
        version: nativeManifest.version,
        tarball: relative(artifactRoot, nativeTarball),
        sha256: await sha256(nativeTarball),
      },
    },
    addon: {
      file: relative(artifactRoot, addonPath),
      sha256: await sha256(addonPath),
    },
  };
  await writeFile(join(artifactRoot, 'provenance.json'), `${JSON.stringify(provenance, null, 2)}\n`);
  await validateProvenance(provenance, { artifactRoot, expectedCommit: commit });
  return provenance;
}

function artifactPath(artifactRoot, path, label) {
  if (typeof path !== 'string' || path.length === 0) throw new Error(`${label} path is missing`);
  const resolved = resolve(artifactRoot, path);
  if (!inside(resolved, resolve(artifactRoot))) throw new Error(`${label} path escapes artifact root`);
  return resolved;
}

export async function validateProvenance(provenance, { artifactRoot, expectedCommit }) {
  if (!/^[0-9a-f]{40}$/.test(provenance?.commit) || provenance.commit !== expectedCommit) {
    throw new Error('provenance commit does not match');
  }
  const target = currentTarget();
  if (provenance.platform !== target.platform) throw new Error('provenance platform does not match current OS');
  const root = provenance.packages?.root;
  const native = provenance.packages?.native;
  if (root?.name !== 'tw-migrate' || native?.name !== target.packageName) throw new Error('provenance package name does not match');
  if (typeof root.version !== 'string' || root.version.length === 0 || native.version !== root.version) {
    throw new Error('provenance package versions do not match');
  }
  for (const [label, entry] of [['root tarball', root], ['native tarball', native]]) {
    const path = artifactPath(artifactRoot, entry.tarball, label);
    if (!/^[0-9a-f]{64}$/.test(entry.sha256) || await sha256(path) !== entry.sha256) {
      throw new Error(`${label} digest does not match provenance`);
    }
  }
  const addon = artifactPath(artifactRoot, provenance.addon?.file, 'addon');
  if (basename(addon) !== target.addon) throw new Error('provenance addon does not match platform');
  if (!/^[0-9a-f]{64}$/.test(provenance.addon?.sha256) || await sha256(addon) !== provenance.addon.sha256) {
    throw new Error('addon digest does not match provenance');
  }
  return provenance;
}

async function installedPackage(path, nodeModules, checkoutRoot, expectedName, version) {
  if ((await lstat(path)).isSymbolicLink()) throw new Error(`${expectedName} must not be a workspace symlink`);
  const resolved = await realpath(path);
  if (!inside(resolved, nodeModules)) throw new Error(`${expectedName} resolved outside driver node_modules`);
  if (inside(resolved, checkoutRoot)) throw new Error(`${expectedName} resolved inside checkout`);
  const manifest = JSON.parse(await readFile(join(resolved, 'package.json'), 'utf8'));
  if (manifest.name !== expectedName || manifest.version !== version) {
    throw new Error(`${expectedName} installed identity does not match provenance`);
  }
  return resolved;
}

export async function assertInstalledLayout({ driverRoot, checkoutRoot, expected }) {
  const target = currentTarget();
  if (expected.platform !== target.platform) throw new Error('installed platform does not match current OS');
  const driver = await realpath(driverRoot);
  const nodeModules = await realpath(join(driver, 'node_modules'));
  const checkout = await realpath(checkoutRoot);
  const root = await installedPackage(join(nodeModules, 'tw-migrate'), nodeModules, checkout, 'tw-migrate', expected.version);
  const native = await installedPackage(join(nodeModules, target.packageName), nodeModules, checkout, target.packageName, expected.version);
  const addon = join(native, target.addon);
  if (!(await lstat(addon)).isFile()) throw new Error(`installed addon is missing: ${addon}`);
  if (await sha256(addon) !== expected.addonSha256) throw new Error('installed addon digest does not match provenance');
  return { root, native, addon };
}

async function publisherToken(registryUrl) {
  const name = `ecosystem-${randomUUID()}`;
  const response = await fetch(`${registryUrl}/-/user/org.couchdb.user:${name}`, {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name, password: randomUUID(), type: 'user', roles: [] }),
  });
  const body = await response.json();
  if (!response.ok || typeof body.token !== 'string') throw new Error(`could not create registry publisher: ${response.status}`);
  return body.token;
}

export async function publishPackages(provenance, artifactRoot, registryUrl) {
  const logPath = join(artifactRoot, 'publish.log');
  const token = await publisherToken(registryUrl);
  const auth = `--//${new URL(registryUrl).host}/:_authToken=${token}`;
  for (const entry of [provenance.packages.native, provenance.packages.root]) {
    await run(process.platform === 'win32' ? 'npm.cmd' : 'npm', [
      'publish', artifactPath(artifactRoot, entry.tarball, `${entry.name} tarball`),
      '--registry', registryUrl,
      '--ignore-scripts',
      auth,
    ], { cwd: artifactRoot, logPath });
  }
}

export function packageUploadRoot(artifactRoot) {
  return `${resolve(artifactRoot)}-upload`;
}

export async function preparePackageUpload(provenance, artifactRoot, uploadRoot) {
  await rm(uploadRoot, { recursive: true, force: true });
  await mkdir(uploadRoot, { recursive: true });
  const entries = ['provenance.json', provenance.packages.root.tarball, provenance.packages.native.tarball, provenance.addon.file];
  let bytes = 0;
  for (const entry of entries) {
    const source = artifactPath(artifactRoot, entry, entry);
    const stat = await lstat(source);
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`package upload is not a regular file: ${entry}`);
    bytes += stat.size;
    if (bytes > 100 * 1024 * 1024) throw new Error('package upload exceeds 100 MiB');
    const destination = join(uploadRoot, entry);
    await mkdir(dirname(destination), { recursive: true });
    await cp(source, destination);
  }
  return entries;
}

export async function runPackageSmoke({ repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..'), artifactRoot }) {
  if (!artifactRoot) throw new Error('package smoke requires an explicit artifact root');
  artifactRoot = resolve(artifactRoot);
  await mkdir(artifactRoot, { recursive: true });
  const runRoot = await mkdtemp(join(tmpdir(), 'tw-migrate-package-smoke-'));
  let registry;
  try {
    const provenance = await stagePackages({ repoRoot, artifactRoot });
    const registryRoot = join(runRoot, 'registry');
    registry = await startRegistry({ root: registryRoot, artifactRoot, allowPublish: true });
    await publishPackages(provenance, artifactRoot, registry.url);
    await registry.stop();
    registry = await startRegistry({ root: registryRoot, artifactRoot, allowPublish: false });

    const driverRoot = join(runRoot, 'driver');
    await mkdir(driverRoot);
    await writeFile(join(driverRoot, 'package.json'), '{"private":true}\n');
    await run(process.platform === 'win32' ? 'npm.cmd' : 'npm', [
      'install', `${provenance.packages.root.name}@${provenance.packages.root.version}`,
      '--registry', registry.url,
      '--ignore-scripts',
      '--no-audit',
      '--no-fund',
      '--fetch-retries=0',
    ], { cwd: driverRoot, logPath: join(artifactRoot, 'install.log') });
    await registry.stop();
    registry = undefined;

    const installed = await assertInstalledLayout({
      driverRoot,
      checkoutRoot: repoRoot,
      expected: {
        version: provenance.packages.root.version,
        platform: provenance.platform,
        addonSha256: provenance.addon.sha256,
      },
    });
    const module = await import(`${pathToFileURL(join(installed.root, 'index.js')).href}?smoke=${Date.now()}`);
    assert.equal(typeof module.migrate, 'function', 'installed package must export migrate()');
    return { provenance, installed };
  } catch (error) {
    await appendFile(join(artifactRoot, 'smoke-error.log'), `${error.stack ?? error}\n`).catch(() => {});
    throw error;
  } finally {
    await registry?.stop().catch(() => {});
    await rm(runRoot, { recursive: true, force: true });
  }
}

async function main() {
  const [mode, flag, artifactRoot, ...rest] = process.argv.slice(2);
  if (!['smoke', 'stage'].includes(mode) || flag !== '--artifact-root' || !artifactRoot || rest.length > 0) {
    throw new Error('Usage: node ecosystem-ci/packages.js (smoke|stage) --artifact-root <path>');
  }
  if (mode === 'smoke') {
    await runPackageSmoke({ artifactRoot });
    console.log(`Package smoke passed; artifacts: ${resolve(artifactRoot)}`);
  } else {
    const provenance = await stagePackages({ repoRoot: resolve(dirname(fileURLToPath(import.meta.url)), '..'), artifactRoot });
    const uploadRoot = packageUploadRoot(artifactRoot);
    await preparePackageUpload(provenance, artifactRoot, uploadRoot);
    console.log(`Packages staged: ${uploadRoot}`);
  }
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
