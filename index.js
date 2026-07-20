import { createRequire } from 'node:module';
import { chmod, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

import { planMigration } from './native.js';

const SOURCE_EXTENSIONS = new Set(['.js', '.jsx', '.mjs', '.ts', '.tsx', '.mts']);
const IGNORED_DIRECTORIES = new Set(['.git', '.next', 'build', 'dist', 'node_modules']);

export async function migrate(options) {
  if (!options?.cssFile) throw new TypeError('migrate() requires cssFile');

  const cwd = resolve(options.cwd ?? process.cwd());
  const cssPath = resolve(cwd, options.cssFile);
  const leftovers = await collectFiles(cwd, (path) => basename(path).includes('.tw-migrate-'));
  if (leftovers.length > 0) {
    const listed = leftovers.map((path) => `  ${relative(cwd, path)}`).join('\n');
    throw new Error(
      `Found leftover tw-migrate files from an interrupted run:\n${listed}\n` +
        'Restore each ".<name>.tw-migrate-backup-*" file by renaming it back to "<name>", ' +
        'delete any remaining ".<name>.tw-migrate-*" staging files, then re-run.',
    );
  }
  const cssSource = await readFile(cssPath, 'utf8');
  const [sourcePaths, tailwindCss] = await Promise.all([
    collectFiles(cwd, (path) => SOURCE_EXTENSIONS.has(extension(path))),
    resolveTailwindEntry(cwd, options.tailwindCss),
  ]);
  const files = await Promise.all(
    sourcePaths.map(async (path) => ({ path, source: await readFile(path, 'utf8') })),
  );

  const tailwind = await loadTailwind(cwd, tailwindCss);
  const plan = JSON.parse(
    planMigration(
      JSON.stringify({
        cssPath,
        cssSource,
        cssModuleId: relative(cwd, cssPath),
        tailwindPath: tailwind.path,
        tailwindSource: tailwind.css,
        utilityPrefix: tailwind.designSystem.theme.prefix,
        themeTokens: tailwind.themeTokens,
        files,
      }),
    ),
  );
  await validateCandidates(tailwind, plan.candidates);

  const originals = new Map([
    ...files.map((file) => [file.path, file.source]),
    [cssPath, cssSource],
    [tailwind.path, tailwind.css],
  ]);
  const changed = plan.files
    .map((file) => ({ ...file, before: originals.get(file.path) }))
    .filter((file) => file.before !== undefined && file.before !== file.source);
  const deleted = plan.deletedFiles.map((path) => ({ path, before: originals.get(path) }));
  const operations = [...changed, ...deleted].sort((left, right) => left.path.localeCompare(right.path));
  const changedFiles = operations.map((file) => relative(cwd, file.path));
  const diff = operations
    .map((file) => unifiedDiff(relative(cwd, file.path), file.before, 'source' in file ? file.source : ''))
    .join('');

  if (options.write && operations.length > 0) await writeChanges(changed, deleted);

  return {
    changedFiles,
    diff,
    convertedRules: plan.convertedRules,
    retainedRules: plan.retainedRules,
    rules: plan.rules,
    candidates: plan.candidates,
    warnings: plan.warnings.map((warning) => ({ ...warning, file: relative(cwd, warning.file) })),
  };
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

async function resolveTailwindEntry(cwd, configuredPath) {
  if (configuredPath) {
    const path = resolve(cwd, configuredPath);
    await readFile(path, 'utf8');
    return path;
  }

  const cssFiles = await collectFiles(cwd, (path) => path.endsWith('.css'));
  const entries = [];
  for (const path of cssFiles) {
    const source = await readFile(path, 'utf8');
    if (/@(import\s+["']tailwindcss(?:\/[^"']*)?["']|tailwind\s+utilities)/.test(source)) {
      entries.push(path);
    }
  }
  if (entries.length === 0) throw new Error('No Tailwind v4 CSS entry was found. Pass --tailwind-css.');
  if (entries.length > 1) throw new Error('Multiple Tailwind CSS entries were found. Pass --tailwind-css.');
  return entries[0];
}

async function loadTailwind(cwd, tailwindCss) {
  const projectRequire = createRequire(join(cwd, 'package.json'));
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
  const { __unstable__loadDesignSystem: loadDesignSystem } =
    tailwindModule.default ?? tailwindModule;
  const css = await readFile(tailwindCss, 'utf8');
  const base = dirname(tailwindCss);
  const loadModule = createModuleLoader();
  const loadStylesheet = createStylesheetLoader(projectRequire, packagePath);
  const defaultTheme = await readFile(join(dirname(packagePath), 'theme.css'), 'utf8');
  const themeTokens = {
    ...extractThemeTokens(defaultTheme),
    ...(await extractThemeTokensFromGraph(css, base, loadStylesheet)),
  };
  const designSystem = await loadDesignSystem(css, {
    base,
    loadModule,
    loadStylesheet,
  });
  return {
    designSystem,
    css,
    path: tailwindCss,
    themeTokens,
  };
}

function extractThemeTokens(css) {
  const tokens = {};
  for (const block of css.matchAll(/@theme[^\{]*\{([^}]*)\}/gs)) {
    for (const match of block[1].matchAll(/--([\w-]+):\s*([^;{}]+);/g)) {
      tokens[match[1]] = match[2].trim();
    }
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
  for (const candidate of candidates) {
    if (tailwind.designSystem.candidatesToCss([candidate])[0] === null) {
      throw new Error(`Tailwind did not generate CSS for candidate: ${candidate}`);
    }
  }
}

function createModuleLoader() {
  return async (id, base) => {
    const path = createRequire(join(base, 'package.json')).resolve(id);
    const imported = await import(pathToFileURL(path));
    return { path, base: dirname(path), module: imported.default ?? imported };
  };
}

function createStylesheetLoader(projectRequire, tailwindPackagePath) {
  const tailwindRoot = dirname(tailwindPackagePath);
  return async (id, base) => {
    let path;
    if (id === 'tailwindcss') path = join(tailwindRoot, 'index.css');
    else if (id.startsWith('tailwindcss/')) {
      const subpath = id.slice('tailwindcss/'.length);
      path = join(tailwindRoot, subpath.endsWith('.css') ? subpath : `${subpath}.css`);
    }
    else if (id.startsWith('.') || isAbsolute(id)) path = resolve(base, id);
    else path = projectRequire.resolve(id);
    return { content: await readFile(path, 'utf8'), base: dirname(path) };
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
  const installed = [];
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
    for (const [temporaryPath, change] of staged) {
      await rename(temporaryPath, change.path);
      installed.push(change.path);
    }
    succeeded = true;
  } finally {
    if (succeeded) {
      await Promise.all(backups.map(([backupPath]) => rm(backupPath, { force: true })));
    } else {
      // Restoring a backup atomically replaces any installed content at the
      // original path, so installed files never need a separate removal step.
      // Best effort per file: one failed restore must not strand the rest;
      // the startup leftover scan reports anything that could not be restored.
      for (const [backupPath, originalPath] of backedUp.reverse()) {
        try {
          await rename(backupPath, originalPath);
        } catch {}
      }
    }
    await Promise.all(staged.map(([temporaryPath]) => rm(temporaryPath, { force: true })));
  }
}
