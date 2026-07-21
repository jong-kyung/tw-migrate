import { execFile } from 'node:child_process';
import { createRequire } from 'node:module';
import { chmod, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

import { planBatchMigration } from './native.js';

const run = promisify(execFile);
const SOURCE_EXTENSIONS = new Set(['.js', '.jsx', '.mjs', '.cjs', '.ts', '.tsx', '.mts', '.cts']);
const IGNORED_DIRECTORIES = new Set(['.git', '.next', 'build', 'dist', 'node_modules']);

export async function migrate(options = {}) {
  if (options.cssFile && options.workspaces) {
    throw new TypeError('cssFile cannot be combined with workspaces');
  }
  if (options.tailwindCss && options.workspaces) {
    throw new TypeError('tailwindCss cannot be combined with workspaces');
  }

  const cwd = resolve(options.cwd ?? process.cwd());
  const currentPackage = await findPackageRoot(cwd);
  const gitRoot = await findGitRoot(currentPackage);
  const workspaceRoot = gitRoot && !(await isIgnoredByGit(gitRoot, currentPackage))
    ? gitRoot
    : currentPackage;
  const allPaths = await discoverFiles(workspaceRoot, workspaceRoot === gitRoot);
  const explicitCss = options.cssFile ? resolve(cwd, options.cssFile) : undefined;
  const configuredEntry = options.tailwindCss ? resolve(cwd, options.tailwindCss) : undefined;
  if (explicitCss && extension(explicitCss) !== '.css') {
    throw new TypeError('Only .css files can be migrated');
  }
  if (configuredEntry && extension(configuredEntry) !== '.css') {
    throw new TypeError('The Tailwind CSS entry must be a .css file');
  }
  if (configuredEntry && !(await stat(configuredEntry)).isFile()) {
    throw new TypeError('The Tailwind CSS entry must be a file');
  }
  if (explicitCss && !isWithin(currentPackage, explicitCss)) {
    throw new TypeError('The selected CSS file must belong to the current package');
  }
  for (const path of [explicitCss, configuredEntry]) {
    if (path && !allPaths.includes(path)) allPaths.push(path);
  }
  allPaths.sort();

  const allPackageRoots = await discoverPackageRoots(workspaceRoot, allPaths);
  if (!allPackageRoots.includes(currentPackage)) allPackageRoots.push(currentPackage);
  for (const path of [explicitCss, configuredEntry]) {
    if (!path) continue;
    const owner = await findPackageRoot(dirname(path));
    if (!allPackageRoots.includes(owner)) allPackageRoots.push(owner);
  }
  allPackageRoots.sort();
  if (explicitCss && owningPackage(explicitCss, allPackageRoots) !== currentPackage) {
    throw new TypeError('The selected CSS file must belong to the current package');
  }
  if (configuredEntry && owningPackage(configuredEntry, allPackageRoots) !== currentPackage) {
    throw new TypeError('The Tailwind CSS entry must belong to the current package');
  }
  const selectedPackages = options.workspaces ? allPackageRoots : [currentPackage];
  const writablePackages = new Set(selectedPackages);
  const snapshots = new Map();

  const leftovers = new Set();
  for (const packageRoot of selectedPackages) {
    for (const path of await collectFiles(packageRoot, (path) => basename(path).includes('.tw-migrate-'))) {
      leftovers.add(path);
    }
  }
  if (leftovers.size > 0) {
    const listed = [...leftovers].sort().map((path) => `  ${relative(cwd, path)}`).join('\n');
    throw new Error(
      `Found leftover tw-migrate files from an interrupted run:\n${listed}\n` +
        'Restore each ".<name>.tw-migrate-backup-*" file by renaming it back to "<name>", ' +
        'delete any remaining ".<name>.tw-migrate-*" staging files, then re-run.',
    );
  }

  const cssPaths = allPaths.filter((path) => path.endsWith('.css'));
  const sourcePaths = allPaths.filter((path) => SOURCE_EXTENSIONS.has(extension(path)));
  const [cssSources, sourceFiles] = await Promise.all([
    readSources(cssPaths, snapshots),
    Promise.all(sourcePaths.map(async (path) => ({ path, source: await snapshotFile(snapshots, path) }))),
    ...selectedPackages.map((packageRoot) => snapshotFile(snapshots, join(packageRoot, 'package.json'))),
  ]);
  if (explicitCss && !cssSources.has(explicitCss)) {
    cssSources.set(explicitCss, await snapshotFile(snapshots, explicitCss));
  }
  if (configuredEntry && !cssSources.has(configuredEntry)) {
    cssSources.set(configuredEntry, await snapshotFile(snapshots, configuredEntry));
  }

  const failures = [];
  const plans = [];
  const sortedPackages = [...selectedPackages].sort();
  for (const packageRoot of sortedPackages) {
    const ownedCss = [...cssSources.keys()].filter(
      (path) => owningPackage(path, allPackageRoots) === packageRoot,
    );
    if (ownedCss.length === 0) continue;

    let tailwindPath;
    let tailwindEntries;
    try {
      ({ path: tailwindPath, entries: tailwindEntries } = resolveTailwindEntry(
        ownedCss,
        cssSources,
        configuredEntry,
      ));
    } catch (error) {
      if (!options.force) throw error;
      failures.push(packageFailure(workspaceRoot, packageRoot, error));
      continue;
    }

    const excludedEntries = new Set([...tailwindEntries, tailwindPath]);
    const targets = explicitCss
      ? [explicitCss]
      : ownedCss.filter((path) => !excludedEntries.has(path));
    if (targets.length === 0) continue;
    if (targets.some((path) => excludedEntries.has(path))) {
      throw new Error('The Tailwind CSS entry cannot be migrated.');
    }

    let tailwind;
    try {
      tailwind = await loadTailwind(packageRoot, tailwindPath, snapshots, workspaceRoot);
    } catch (error) {
      if (!options.force) throw error;
      failures.push(packageFailure(workspaceRoot, packageRoot, error));
      continue;
    }

    const files = sourceFiles.map((file) => {
      const owner = owningPackage(file.path, allPackageRoots);
      return {
        ...file,
        writable: options.workspaces ? writablePackages.has(owner) : owner === packageRoot,
      };
    });
    const stylesheets = targets.sort().map((cssPath) => ({
      cssPath,
      cssSource: cssSources.get(cssPath),
      cssModuleId: relative(packageRoot, cssPath),
      cssDependents: findCssDependents(cssSources, cssPath),
    }));
    let plan;
    try {
      plan = JSON.parse(
        planBatchMigration(
          JSON.stringify({
            stylesheets,
            tailwindPath: tailwind.path,
            tailwindSource: tailwind.css,
            utilityPrefix: tailwind.designSystem.theme.prefix,
            themeTokens: tailwind.themeTokens,
            files,
          }),
        ),
      );
    } catch (error) {
      if (!options.force || !isRecoverablePlanningError(error)) throw error;
      failures.push(packageFailure(workspaceRoot, packageRoot, error));
      continue;
    }

    await validateCandidates(tailwind, plan.candidates);
    plans.push({ packageRoot, plan, tailwind });
  }

  const originals = new Map([
    ...cssSources,
    ...sourceFiles.map((file) => [file.path, file.source]),
  ]);
  const filesByPath = new Map();
  const deletedPaths = new Set();
  const candidates = new Set();
  const rules = [];
  const warnings = [];
  let convertedRules = 0;
  let retainedRules = 0;

  for (const { plan } of plans) {
    for (const file of plan.files) {
      if (!originals.has(file.path)) throw new Error(`Planned file is outside the source snapshot: ${file.path}`);
      if (filesByPath.has(file.path) || deletedPaths.has(file.path)) {
        throw new Error(`Multiple package groups planned changes for ${file.path}`);
      }
      filesByPath.set(file.path, file);
    }
    for (const path of plan.deletedFiles) {
      if (!originals.has(path)) throw new Error(`Planned deletion is outside the source snapshot: ${path}`);
      if (filesByPath.has(path) || deletedPaths.has(path)) {
        throw new Error(`Multiple package groups planned changes for ${path}`);
      }
      deletedPaths.add(path);
    }
    for (const candidate of plan.candidates) candidates.add(candidate);
    convertedRules += plan.convertedRules;
    retainedRules += plan.retainedRules;
    rules.push(...plan.rules);
    warnings.push(...plan.warnings);
  }

  const changed = [...filesByPath.values()]
    .map((file) => ({ ...file, before: originals.get(file.path) }))
    .filter((file) => file.before !== file.source)
    .sort((left, right) => left.path.localeCompare(right.path));
  const deleted = [...deletedPaths]
    .map((path) => ({ path, before: originals.get(path) }))
    .sort((left, right) => left.path.localeCompare(right.path));
  const operations = [...changed, ...deleted].sort((left, right) => left.path.localeCompare(right.path));
  const changedFiles = operations.map((file) => relative(cwd, file.path));
  const diff = operations
    .map((file) => unifiedDiff(relative(cwd, file.path), file.before, 'source' in file ? file.source : ''))
    .join('');

  if (options.write && operations.length > 0) {
    await verifySnapshots(snapshots);
    await writeChanges(changed, deleted);
  }

  warnings.sort((left, right) =>
    left.file.localeCompare(right.file) || left.start - right.start || left.end - right.end || left.code.localeCompare(right.code));
  failures.sort((left, right) => left.package.localeCompare(right.package));
  return {
    changedFiles,
    diff,
    convertedRules,
    retainedRules,
    rules,
    candidates: [...candidates].sort(),
    warnings: warnings.map((warning) => ({ ...warning, file: relative(cwd, warning.file) })),
    failures,
  };
}

async function findPackageRoot(start) {
  let directory = start;
  while (true) {
    try {
      await readFile(join(directory, 'package.json'), 'utf8');
      return directory;
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
    const parent = dirname(directory);
    if (parent === directory) throw new Error(`No package.json was found from ${start}`);
    directory = parent;
  }
}

async function findGitRoot(cwd) {
  try {
    const { stdout } = await run('git', ['rev-parse', '--show-toplevel'], { cwd });
    return resolve(stdout.trim());
  } catch {
    return undefined;
  }
}

async function isIgnoredByGit(gitRoot, path) {
  if (gitRoot === path) return false;
  try {
    await run('git', ['check-ignore', '-q', '--', relative(gitRoot, path)], { cwd: gitRoot });
    return true;
  } catch {
    return false;
  }
}

async function discoverFiles(root, useGit) {
  if (!useGit) return collectFiles(root, () => true);

  const { stdout } = await run(
    'git',
    ['ls-files', '-co', '--exclude-standard', '-z', '--', '.'],
    { cwd: root, maxBuffer: 64 * 1024 * 1024 },
  );
  const paths = stdout
    .split('\0')
    .filter(Boolean)
    .map((path) => resolve(root, path))
    .filter((path) => !hasIgnoredDirectory(root, path));
  const existing = await Promise.all(paths.map(async (path) => {
    try {
      return (await stat(path)).isFile() ? path : undefined;
    } catch {
      return undefined;
    }
  }));
  return existing.filter(Boolean).sort();
}

async function discoverPackageRoots(workspaceRoot, paths) {
  const roots = paths
    .filter((path) => basename(path) === 'package.json')
    .map(dirname);
  try {
    await readFile(join(workspaceRoot, 'package.json'), 'utf8');
    roots.push(workspaceRoot);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  return [...new Set(roots)].sort();
}

function owningPackage(path, packageRoots) {
  return packageRoots
    .filter((root) => isWithin(root, path))
    .sort((left, right) => right.length - left.length)[0];
}

function isWithin(root, path) {
  return path === root || path.startsWith(`${root}${sep}`);
}

function hasIgnoredDirectory(root, path) {
  return relative(root, path).split(/[\\/]/).some((part) => IGNORED_DIRECTORIES.has(part));
}

function isProjectInput(workspaceRoot, path) {
  return isWithin(workspaceRoot, path)
    && !relative(workspaceRoot, path).split(/[\\/]/).includes('node_modules');
}

async function snapshotFile(snapshots, path) {
  const source = await readFile(path, 'utf8');
  if (snapshots.has(path) && snapshots.get(path) !== source) {
    throw new Error(`Source changed during planning: ${path}`);
  }
  snapshots.set(path, source);
  return source;
}

function packageFailure(workspaceRoot, packageRoot, error) {
  return {
    package: relative(workspaceRoot, packageRoot) || '.',
    message: error instanceof Error ? error.message : String(error),
  };
}

function isRecoverablePlanningError(error) {
  const message = error instanceof Error ? error.message : String(error);
  if (message.startsWith('Failed to parse edited CSS')) return false;
  return message.startsWith('Failed to parse ')
    || message.startsWith('Failed to analyze ')
    || message.startsWith('Unsupported source file ');
}

async function readSources(paths, snapshots) {
  return new Map(await Promise.all(paths.map(async (path) => [path, await snapshotFile(snapshots, path)])));
}

async function collectFiles(root, include) {
  const files = [];
  async function visit(directory) {
    const entries = await readdir(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        if (!IGNORED_DIRECTORIES.has(entry.name)) await visit(path);
      } else if (entry.isFile() && include(path)) {
        files.push(path);
      }
    }
  }
  await visit(root);
  return files;
}

function extension(path) {
  const match = /\.[^.\/]+$/.exec(path);
  return match?.[0] ?? '';
}

function stripCssComments(source) {
  return source.replace(/\/\*[\s\S]*?\*\//g, '');
}

function findCssDependents(cssSources, cssPath) {
  if (!cssPath.endsWith('.module.css')) return [];
  const dependents = [];
  for (const [path, rawSource] of cssSources) {
    if (path === cssPath) continue;
    const source = stripCssComments(rawSource);
    const references = [
      ...source.matchAll(
        /(?:composes\s*:[^;{}]*?\bfrom\s+|@import\s+(?:url\(\s*)?)["']([^"']+)["']/g,
      ),
      ...source.matchAll(/@import\s+url\(\s*([^"'()\s]+)\s*\)/g),
    ];
    if (references.some((match) => resolve(dirname(path), match[1]) === cssPath)) dependents.push(path);
  }
  return dependents.sort();
}

function resolveTailwindEntry(cssPaths, cssSources, configuredPath) {
  const entries = cssPaths.filter((path) => {
    const source = stripCssComments(cssSources.get(path));
    return /@import\s+["']tailwindcss(?:\/[^"']*)?["']/.test(source);
  });
  if (configuredPath) return { path: configuredPath, entries };
  if (entries.length === 0) throw new Error('No Tailwind v4 CSS entry was found. Pass --tailwind-css.');
  if (entries.length > 1) throw new Error('Multiple Tailwind CSS entries were found. Pass --tailwind-css.');
  return { path: entries[0], entries };
}

async function loadTailwind(packageRoot, tailwindCss, snapshots, workspaceRoot) {
  const projectRequire = createRequire(join(packageRoot, 'package.json'));
  let packagePath;
  try {
    packagePath = projectRequire.resolve('tailwindcss/package.json');
  } catch {
    throw new Error('Tailwind v4 must be installed in the target project.');
  }
  const packageJson = JSON.parse(await readFile(packagePath, 'utf8'));
  if (!String(packageJson.version).startsWith('4.')) throw new Error(`Tailwind v4 is required; found ${packageJson.version}.`);

  const modulePath = projectRequire.resolve('tailwindcss');
  const tailwindModule = await import(pathToFileURL(modulePath));
  const { __unstable__loadDesignSystem: loadDesignSystem } = tailwindModule.default ?? tailwindModule;
  const css = await snapshotFile(snapshots, tailwindCss);
  const base = dirname(tailwindCss);
  const loadModule = createModuleLoader(snapshots, workspaceRoot);
  const loadStylesheet = createStylesheetLoader(projectRequire, packagePath, snapshots, workspaceRoot);
  const defaultTheme = await readFile(join(dirname(packagePath), 'theme.css'), 'utf8');
  const themeTokens = {
    ...extractThemeTokens(defaultTheme),
    ...(await extractThemeTokensFromGraph(css, base, loadStylesheet)),
  };
  const designSystem = await loadDesignSystem(css, { base, loadModule, loadStylesheet });
  return { designSystem, css, path: tailwindCss, themeTokens };
}

function extractThemeTokens(css) {
  const tokens = {};
  for (const block of css.matchAll(/@theme[^\{]*\{([^}]*)\}/gs)) {
    for (const match of block[1].matchAll(/--([\w-]+):\s*([^;{}]+);/g)) tokens[match[1]] = match[2].trim();
  }
  return tokens;
}

async function extractThemeTokensFromGraph(css, base, loadStylesheet, seen = new Set()) {
  const tokens = {};
  for (const match of css.matchAll(/@import\s+["']([^"']+)["']/g)) {
    const key = `${base}\0${match[1]}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const loaded = await loadStylesheet(match[1], base);
    Object.assign(tokens, await extractThemeTokensFromGraph(loaded.content, loaded.base, loadStylesheet, seen));
  }
  return Object.assign(tokens, extractThemeTokens(css));
}

async function validateCandidates(tailwind, candidates) {
  const generated = tailwind.designSystem.candidatesToCss(candidates);
  const invalid = candidates.find((_, index) => generated[index] === null);
  if (invalid) throw new Error(`Tailwind did not generate CSS for candidate: ${invalid}`);
}

function createModuleLoader(snapshots, workspaceRoot) {
  return async (id, base) => {
    const path = createRequire(join(base, 'package.json')).resolve(id);
    if (isProjectInput(workspaceRoot, path)) await snapshotFile(snapshots, path);
    const imported = await import(pathToFileURL(path));
    return { path, base: dirname(path), module: imported.default ?? imported };
  };
}

function createStylesheetLoader(projectRequire, tailwindPackagePath, snapshots, workspaceRoot) {
  const tailwindRoot = dirname(tailwindPackagePath);
  return async (id, base) => {
    let path;
    if (id === 'tailwindcss') path = join(tailwindRoot, 'index.css');
    else if (id.startsWith('tailwindcss/')) {
      const subpath = id.slice('tailwindcss/'.length);
      path = join(tailwindRoot, subpath.endsWith('.css') ? subpath : `${subpath}.css`);
    } else if (id.startsWith('.') || isAbsolute(id)) path = resolve(base, id);
    else path = projectRequire.resolve(id);
    const content = isProjectInput(workspaceRoot, path)
      ? await snapshotFile(snapshots, path)
      : await readFile(path, 'utf8');
    return { content, base: dirname(path) };
  };
}

function unifiedDiff(path, before, after) {
  const oldLines = before.split('\n');
  const newLines = after.split('\n');
  return [
    `--- a/${path}\n`,
    `+++ b/${path}\n`,
    `@@ -1,${oldLines.length} +1,${newLines.length} @@\n`,
    ...oldLines.map((line, index) => `${index === oldLines.length - 1 && line === '' ? ' ' : '-'}${line}\n`),
    ...newLines.map((line, index) => `${index === newLines.length - 1 && line === '' ? ' ' : '+'}${line}\n`),
  ].join('');
}

async function verifySnapshots(snapshots) {
  for (const [path, before] of [...snapshots].sort(([left], [right]) => left.localeCompare(right))) {
    let current;
    try {
      current = await readFile(path, 'utf8');
    } catch (error) {
      throw new Error(`Source changed after planning: ${path} (${error.code ?? error.message})`);
    }
    if (current !== before) throw new Error(`Source changed after planning: ${path}`);
  }
}

async function writeChanges(changes, deletions) {
  const token = `${process.pid}-${Date.now()}`;
  const staged = changes.map((change, index) => [
    join(dirname(change.path), `.${basename(change.path)}.tw-migrate-${token}-${index}`),
    change,
  ]);
  const backups = [...changes, ...deletions].map((change, index) => [
    join(dirname(change.path), `.${basename(change.path)}.tw-migrate-backup-${token}-${index}`),
    change.path,
  ]);
  const backedUp = [];
  let succeeded = false;
  try {
    for (const [temporaryPath, change] of staged) {
      const { mode } = await stat(change.path);
      await writeFile(temporaryPath, change.source);
      await chmod(temporaryPath, mode & 0o777);
    }
    for (const [backupPath, originalPath] of backups) {
      await rename(originalPath, backupPath);
      backedUp.push([backupPath, originalPath]);
    }
    for (const [temporaryPath, change] of staged) await rename(temporaryPath, change.path);
    succeeded = true;
  } finally {
    if (succeeded) {
      await Promise.all(backups.map(([backupPath]) => rm(backupPath, { force: true })));
    } else {
      for (const [backupPath, originalPath] of backedUp.reverse()) {
        try {
          await rename(backupPath, originalPath);
        } catch {}
      }
    }
    await Promise.all(staged.map(([temporaryPath]) => rm(temporaryPath, { force: true })));
  }
}
