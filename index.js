import { execFile } from 'node:child_process';
import { createRequire } from 'node:module';
import { chmod, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

import { parseHtmlSource } from './html.js';
import { planBatchMigration } from './native.js';
import {
  compileLessEntry,
  compileSassEntry,
  isPreprocessorPath,
  isSassPath,
  loadProjectLess,
  loadProjectSass,
} from './style-compiler.js';

const run = promisify(execFile);
const SOURCE_EXTENSIONS = new Set(['.html', '.js', '.jsx', '.mjs', '.cjs', '.ts', '.tsx', '.mts', '.cts']);
const STYLESHEET_SYNTAX = new Map([
  ['.css', 'css'],
  ['.scss', 'scss'],
  ['.sass', 'sass'],
  ['.less', 'less'],
]);
const IGNORED_DIRECTORIES = new Set(['.git', '.next', 'build', 'dist', 'node_modules']);
const RECOVERABLE_INPUT_ERROR = 'TW_MIGRATE_RECOVERABLE_INPUT:';

export async function migrate(options = {}) {
  if ('cssFile' in options) {
    throw new TypeError('cssFile has been replaced by styleFile');
  }
  if (options.styleFile && options.workspaces) {
    throw new TypeError('styleFile cannot be combined with workspaces');
  }
  if (options.tailwindCss && options.workspaces) {
    throw new TypeError('tailwindCss cannot be combined with workspaces');
  }

  const scope = await resolveScope(options);
  const { cwd, workspaceRoot, selectedPackages, explicitStyle, configuredEntry } = scope;

  // A prior interrupted run's scope is unknowable from the current flags
  // (it may have covered other packages), so scan the whole workspace root.
  const leftovers = await collectFiles(workspaceRoot, (path) => basename(path).includes('.tw-migrate-'));
  if (leftovers.length > 0) {
    const listed = leftovers.sort().map((path) => `  ${normalizedRelativePath(cwd, path)}`).join('\n');
    throw new Error(
      `Found leftover tw-migrate files from an interrupted run:\n${listed}\n` +
        'Restore each ".<name>.tw-migrate-backup-*" file by renaming it back to "<name>", ' +
        'delete any remaining ".<name>.tw-migrate-*" staging files, then re-run.',
    );
  }

  const snapshots = new Map();
  const stylePaths = scope.scannedPaths.filter(isStylesheetPath);
  const sourcePaths = scope.scannedPaths.filter((path) => SOURCE_EXTENSIONS.has(extension(path)));
  const [styleSources, sourceCandidates] = await Promise.all([
    readSources(stylePaths, snapshots),
    Promise.all(sourcePaths.map(async (path) => {
      const source = await readFile(path, 'utf8');
      // Scan-only sources matter solely as potential CSS Module references,
      // and every supported reference names the module literally. Unrelated
      // gitignored files (coverage output, generated bundles) must reach
      // neither the parser nor the snapshot ledger.
      if (!scope.targetable.has(path)
        && (extension(path) === '.html' || !mentionsStylesheetModule(source))) return undefined;
      return { path, source: recordSnapshot(snapshots, path, source) };
    })),
    ...selectedPackages.map((packageRoot) => snapshotFile(snapshots, join(packageRoot, 'package.json'))),
  ]);
  const sourceFiles = sourceCandidates.filter(Boolean);
  if (explicitStyle && !styleSources.has(explicitStyle)) {
    styleSources.set(explicitStyle, await snapshotFile(snapshots, explicitStyle));
  }
  if (configuredEntry && !styleSources.has(configuredEntry)) {
    styleSources.set(configuredEntry, await snapshotFile(snapshots, configuredEntry));
  }

  const context = {
    ...scope,
    options,
    snapshots,
    styleSources,
    sourceFiles,
    styleDependents: indexStylesheetDependents(styleSources),
  };
  const failures = [];
  const plans = [];
  for (const packageRoot of selectedPackages) {
    const result = await planPackage(context, packageRoot);
    if (result.failure) failures.push(result.failure);
    else if (result.plan) plans.push(result.plan);
  }

  const originals = new Map([
    ...styleSources,
    ...sourceFiles.map((file) => [file.path, file.source]),
  ]);
  const { filesByPath, deletedPaths, candidates, rules, warnings, convertedRules, retainedRules } =
    mergePlans(plans, originals);

  const changed = [...filesByPath.values()]
    .map((file) => ({ ...file, before: originals.get(file.path) }))
    .filter((file) => file.before !== file.source)
    .sort((left, right) => left.path.localeCompare(right.path));
  const deleted = [...deletedPaths]
    .map((path) => ({ path, before: originals.get(path) }))
    .sort((left, right) => left.path.localeCompare(right.path));
  const operations = [...changed, ...deleted].sort((left, right) => left.path.localeCompare(right.path));
  const changedFiles = operations.map((file) => normalizedRelativePath(cwd, file.path));
  const diff = operations
    .map((file) => unifiedDiff(normalizedRelativePath(cwd, file.path), file.before, 'source' in file ? file.source : ''))
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
    warnings: warnings.map((warning) => ({ ...warning, file: normalizedRelativePath(cwd, warning.file) })),
    failures,
  };
}

async function resolveScope(options) {
  const cwd = resolve(options.cwd ?? process.cwd());
  const currentPackage = await findPackageRoot(cwd);
  const gitRoot = await findGitRoot(currentPackage);
  const workspaceRoot = gitRoot && !(await isIgnoredByGit(gitRoot, currentPackage))
    ? gitRoot
    : currentPackage;
  const allPaths = await discoverFiles(workspaceRoot, workspaceRoot === gitRoot);
  // Ignore filtering scopes what gets migrated, never what gets scanned:
  // gitignored consumers and stylesheets must still block unsafe deletion.
  const scannedPaths = workspaceRoot === gitRoot
    ? [...new Set([...allPaths, ...(await collectFiles(workspaceRoot, isRelevantDiscoveredFile))])]
    : [...allPaths];
  const explicitStyle = options.styleFile ? resolve(cwd, options.styleFile) : undefined;
  const configuredEntry = options.tailwindCss ? resolve(cwd, options.tailwindCss) : undefined;
  if (explicitStyle && !isStylesheetPath(explicitStyle)) {
    throw new TypeError('Only .css, .scss, .sass, and .less files can be migrated');
  }
  if (configuredEntry && extension(configuredEntry) !== '.css') {
    throw new TypeError('The Tailwind CSS entry must be a .css file');
  }
  if (configuredEntry && !(await stat(configuredEntry)).isFile()) {
    throw new TypeError('The Tailwind CSS entry must be a file');
  }
  if (explicitStyle && !isWithin(currentPackage, explicitStyle)) {
    throw new TypeError('The selected stylesheet must belong to the current package');
  }
  for (const path of [explicitStyle, configuredEntry]) {
    if (path && !allPaths.includes(path)) allPaths.push(path);
    if (path && !scannedPaths.includes(path)) scannedPaths.push(path);
  }
  allPaths.sort();
  scannedPaths.sort();
  const targetable = new Set(allPaths);

  const allPackageRoots = await discoverPackageRoots(workspaceRoot, allPaths);
  if (!allPackageRoots.includes(currentPackage)) allPackageRoots.push(currentPackage);
  for (const path of [explicitStyle, configuredEntry]) {
    if (!path) continue;
    const owner = await findPackageRoot(dirname(path));
    if (!allPackageRoots.includes(owner)) allPackageRoots.push(owner);
  }
  allPackageRoots.sort();
  const pathOwners = new Map(scannedPaths.map((path) => [path, owningPackage(path, allPackageRoots)]));
  if (explicitStyle && pathOwners.get(explicitStyle) !== currentPackage) {
    throw new TypeError('The selected stylesheet must belong to the current package');
  }
  if (configuredEntry && pathOwners.get(configuredEntry) !== currentPackage) {
    throw new TypeError('The Tailwind CSS entry must belong to the current package');
  }
  const selectedPackages = options.workspaces ? allPackageRoots : [currentPackage];
  return {
    cwd,
    workspaceRoot,
    scannedPaths,
    targetable,
    explicitStyle,
    configuredEntry,
    pathOwners,
    selectedPackages,
    writablePackages: new Set(selectedPackages),
  };
}

async function planPackage(context, packageRoot) {
  const {
    options,
    snapshots,
    workspaceRoot,
    explicitStyle,
    configuredEntry,
    styleSources,
    sourceFiles,
    styleDependents,
    pathOwners,
    targetable,
    writablePackages,
  } = context;
  let preparedHtml;
  try {
    preparedHtml = await preparePackageHtml({
      packageRoot,
      sourceFiles,
      styleSources,
      snapshots,
      pathOwners,
    });
  } catch (error) {
    if (!options.force) throw error;
    return { failure: packageFailure(workspaceRoot, packageRoot, error) };
  }
  const packageSources = [
    ...sourceFiles.filter((file) => extension(file.path) !== '.html'),
    ...preparedHtml.files,
  ];
  const ownedStyles = [...styleSources.keys()].filter(
    (path) => pathOwners.get(path) === packageRoot
      && (targetable.has(path) || preparedHtml.stylePaths.has(path)),
  );
  if (ownedStyles.length === 0) return {};

  let tailwindPath;
  let tailwindEntries;
  try {
    ({ path: tailwindPath, entries: tailwindEntries } = resolveTailwindEntry(
      ownedStyles,
      styleSources,
      configuredEntry,
    ));
  } catch (error) {
    if (!options.force) throw error;
    return { failure: packageFailure(workspaceRoot, packageRoot, error) };
  }

  const excludedEntries = new Set([...tailwindEntries, tailwindPath]);
  const targets = explicitStyle
    ? [explicitStyle]
    : ownedStyles.filter((path) =>
      !excludedEntries.has(path)
      && !preparedHtml.generatedPaths.has(path)
      && (!isPreprocessorPath(path)
        || preparedHtml.stylePaths.has(path)
        || packageSources.some((file) => sourceReferencesStyle(file, path))),
    );
  if (targets.length === 0) return {};
  if (targets.some((path) => excludedEntries.has(path))) {
    throw new Error('The Tailwind CSS entry cannot be migrated.');
  }

  let tailwind;
  try {
    tailwind = await loadTailwind(packageRoot, tailwindPath, snapshots, workspaceRoot);
  } catch (error) {
    if (!options.force) throw error;
    return { failure: packageFailure(workspaceRoot, packageRoot, error) };
  }

  const files = packageSources.map((file) => {
    const owner = pathOwners.get(file.path);
    return {
      ...file,
      writable: targetable.has(file.path)
        && (options.workspaces ? writablePackages.has(owner) : owner === packageRoot),
    };
  });
  let stylesheets;
  let sass;
  let less;
  try {
    stylesheets = [];
    for (const stylePath of targets.sort()) {
      const isPartial = isSassPath(stylePath) && basename(stylePath).startsWith('_');
      const stylesheet = {
        cssPath: stylePath,
        cssSource: styleSources.get(stylePath),
        cssModuleId: normalizedRelativePath(packageRoot, stylePath),
        cssDependents: styleDependents.get(stylePath) ?? [],
        syntax: stylesheetSyntax(stylePath),
        isModule: isStylesheetModule(stylePath),
        isPartial,
      };
      let compiled;
      if (isSassPath(stylePath) && !isPartial) {
        sass ??= await loadProjectSass(packageRoot);
        compiled = await compileSassEntry(sass, stylePath);
      } else if (extension(stylePath) === '.less') {
        less ??= await loadProjectLess(packageRoot);
        compiled = await compileLessEntry(less, stylePath, stylesheet.cssSource);
      }
      if (compiled) {
        for (const loadedPath of compiled.loadedPaths) {
          if (!isProjectInput(workspaceRoot, loadedPath)) continue;
          const source = await snapshotFile(snapshots, loadedPath);
          if (!styleSources.has(loadedPath)) styleSources.set(loadedPath, source);
        }
        stylesheet.analysisSource = compiled.css;
        stylesheet.sourceMappings = compiled.sourceMappings;
      }
      stylesheets.push(stylesheet);
    }
  } catch (error) {
    if (!options.force) throw error;
    return { failure: packageFailure(workspaceRoot, packageRoot, error) };
  }

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
    return { failure: packageFailure(workspaceRoot, packageRoot, error) };
  }

  plan.warnings.push(...preparedHtml.warnings);
  await validateCandidates(tailwind, plan.candidates);
  for (const file of plan.files.filter((file) => extension(file.path) === '.html')) {
    parseHtmlSource(file.path, file.source);
  }
  for (const stylesheet of stylesheets.filter((stylesheet) => isPreprocessorPath(stylesheet.cssPath))) {
    const changed = plan.files.find((file) => file.path === stylesheet.cssPath);
    if (!changed && !plan.deletedFiles.includes(stylesheet.cssPath)) continue;
    const source = changed?.source ?? '';
    if (isSassPath(stylesheet.cssPath)) {
      await compileSassEntry(sass, stylesheet.cssPath, source);
    } else {
      await compileLessEntry(less, stylesheet.cssPath, source);
    }
  }
  return { plan };
}

function mergePlans(plans, originals) {
  const filesByPath = new Map();
  const deletedPaths = new Set();
  const candidates = new Set();
  const rules = [];
  const warnings = [];
  let convertedRules = 0;
  let retainedRules = 0;

  for (const plan of plans) {
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
  return { filesByPath, deletedPaths, candidates, rules, warnings, convertedRules, retainedRules };
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
  if (!useGit) return collectFiles(root, isRelevantDiscoveredFile);

  const { stdout } = await run(
    'git',
    ['ls-files', '-co', '--exclude-standard', '-z', '--', '.'],
    { cwd: root, maxBuffer: 64 * 1024 * 1024 },
  );
  const paths = stdout
    .split('\0')
    .filter(Boolean)
    .map((path) => resolve(root, path))
    .filter((path) => !hasIgnoredDirectory(root, path) && isRelevantDiscoveredFile(path));
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

function isRelevantDiscoveredFile(path) {
  return basename(path) === 'package.json'
    || isStylesheetPath(path)
    || SOURCE_EXTENSIONS.has(extension(path));
}

function isStylesheetPath(path) {
  return STYLESHEET_SYNTAX.has(extension(path));
}

function stylesheetSyntax(path) {
  return STYLESHEET_SYNTAX.get(extension(path));
}

function isStylesheetModule(path) {
  const syntax = stylesheetSyntax(path);
  return syntax !== undefined && path.endsWith(`.module.${syntax}`);
}

function mentionsStylesheetModule(source) {
  return [...STYLESHEET_SYNTAX.keys()].some((extension) => source.includes(`.module${extension}`));
}

function sourceReferencesStyle(file, stylePath) {
  let importPath = normalizedRelativePath(dirname(file.path), stylePath);
  if (!importPath.startsWith('.')) importPath = `./${importPath}`;
  return [`'${importPath}'`, `"${importPath}"`, `\`${importPath}\``]
    .some((literal) => file.source.includes(literal));
}

async function preparePackageHtml({
  packageRoot,
  sourceFiles,
  styleSources,
  snapshots,
  pathOwners,
}) {
  const files = [];
  const stylePaths = new Set();
  const generatedPaths = new Set();
  const warnings = [];

  for (const file of sourceFiles.filter(
    (file) => extension(file.path) === '.html' && pathOwners.get(file.path) === packageRoot,
  )) {
    const parsed = parseHtmlSource(file.path, file.source);
    const contexts = [];
    for (const link of parsed.links) {
      const variants = mediaVariants(link.media);
      if (variants === undefined) {
        warnings.push(htmlWarning(
          'unsupported-link-media',
          file.path,
          link.start,
          link.end,
          `The stylesheet link media condition ${JSON.stringify(link.media)} cannot be represented safely.`,
        ));
        continue;
      }
      const linkedPath = localHtmlReference(packageRoot, dirname(file.path), link.href);
      if (!linkedPath || !isWithin(packageRoot, linkedPath)) {
        warnings.push(htmlWarning(
          'unsupported-html-stylesheet-link',
          file.path,
          link.start,
          link.end,
          'Only local package stylesheet links are analyzed.',
        ));
        continue;
      }
      await collectHtmlStyleContexts({
        path: linkedPath,
        variants,
        packageRoot,
        styleSources,
        snapshots,
        pathOwners,
        stylePaths,
        generatedPaths,
        contexts,
        warnings,
        visited: new Set(),
      });
    }

    if (contexts.length > 0) {
      for (const attribute of parsed.dynamicAttributes) {
        warnings.push(htmlWarning(
          'dynamic-html-attribute',
          file.path,
          attribute.start,
          attribute.end,
          'This HTML attribute is not a safely writable quoted literal.',
        ));
      }
    }
    files.push({
      ...file,
      htmlElements: parsed.elements,
      htmlStylesheets: deduplicateHtmlContexts(contexts),
    });
  }

  return { files, stylePaths, generatedPaths, warnings };
}

async function collectHtmlStyleContexts(state) {
  const key = `${state.path}\0${state.variants.join(':')}`;
  if (state.visited.has(key)) return;
  state.visited.add(key);
  if (!isStylesheetPath(state.path)) return;

  let source;
  try {
    source = state.styleSources.get(state.path)
      ?? await snapshotFile(state.snapshots, state.path);
  } catch (error) {
    if (error.code === 'ENOENT') return;
    throw error;
  }
  if (!state.styleSources.has(state.path)) state.styleSources.set(state.path, source);
  state.pathOwners.set(state.path, state.packageRoot);

  if (extension(state.path) === '.css') {
    const mapped = await mappedPreprocessorSource(state.path, source, state);
    if (mapped) {
      state.generatedPaths.add(state.path);
      state.stylePaths.add(mapped);
      state.contexts.push({ cssPath: mapped, variants: state.variants });
      return;
    }
  }

  state.stylePaths.add(state.path);
  state.contexts.push({ cssPath: state.path, variants: state.variants });
  if (extension(state.path) !== '.css') return;

  for (const imported of cssImports(source)) {
    const variants = mediaVariants(imported.media);
    if (variants === undefined) {
      state.warnings.push(htmlWarning(
        'unsupported-link-media',
        state.path,
        imported.start,
        imported.end,
        `The stylesheet import media condition ${JSON.stringify(imported.media)} cannot be represented safely.`,
      ));
      continue;
    }
    const importedPath = localHtmlReference(state.packageRoot, dirname(state.path), imported.href);
    if (!importedPath || !isWithin(state.packageRoot, importedPath)) continue;
    await collectHtmlStyleContexts({
      ...state,
      path: importedPath,
      variants: [...state.variants, ...variants],
    });
  }
}

async function mappedPreprocessorSource(cssPath, cssSource, state) {
  const matches = [...cssSource.matchAll(/\/\*[#@]\s*sourceMappingURL=([^\s*]+)\s*\*\//g)];
  const reference = matches.at(-1)?.[1];
  const mapPath = reference && localHtmlReference(state.packageRoot, dirname(cssPath), reference);
  if (!mapPath || !isWithin(state.packageRoot, mapPath)) return undefined;

  let rawMap;
  try {
    rawMap = await snapshotFile(state.snapshots, mapPath);
  } catch (error) {
    if (error.code === 'ENOENT') return undefined;
    throw error;
  }
  let sourceMap;
  try {
    sourceMap = JSON.parse(rawMap);
  } catch {
    return undefined;
  }
  const sources = (sourceMap.sources ?? [])
    .map((source) => sourceMapSource(mapPath, sourceMap.sourceRoot, source))
    .filter((path) => path && isPreprocessorPath(path) && isWithin(state.packageRoot, path));
  const uniqueSources = [...new Set(sources)];
  const entries = uniqueSources.filter((path) => !basename(path).startsWith('_'));
  if (entries.length !== 1) return undefined;

  const entryPath = entries[0];
  const entrySource = state.styleSources.get(entryPath)
    ?? await snapshotFile(state.snapshots, entryPath);
  if (!state.styleSources.has(entryPath)) state.styleSources.set(entryPath, entrySource);
  state.pathOwners.set(entryPath, state.packageRoot);
  return entryPath;
}

function sourceMapSource(mapPath, sourceRoot, source) {
  try {
    const url = new URL(source);
    return url.protocol === 'file:' ? fileURLToPath(url) : undefined;
  } catch {
    return resolve(dirname(mapPath), sourceRoot ?? '', source);
  }
}

function cssImports(source) {
  const imports = [];
  const withoutComments = stripCssComments(source);
  const pattern = /@import\s+(?:url\(\s*)?(?:["']([^"']+)["']|([^"'()\s;]+))\s*\)?\s*([^;]*);/g;
  for (const match of withoutComments.matchAll(pattern)) {
    imports.push({
      href: match[1] ?? match[2],
      media: match[3].trim(),
      start: match.index,
      end: match.index + match[0].length,
    });
  }
  return imports;
}

function localHtmlReference(packageRoot, base, reference) {
  const path = reference.split(/[?#]/, 1)[0];
  if (!path || path.startsWith('//') || /^[a-z][a-z\d+.-]*:/i.test(path)) return undefined;
  let decoded;
  try {
    decoded = decodeURIComponent(path);
  } catch {
    return undefined;
  }
  return decoded.startsWith('/')
    ? resolve(packageRoot, `.${decoded}`)
    : resolve(base, decoded);
}

function mediaVariants(media) {
  const normalized = media.trim().toLowerCase();
  if (!normalized || normalized === 'all') return [];
  if (normalized === 'print') return ['print'];
  return undefined;
}

function deduplicateHtmlContexts(contexts) {
  const seen = new Set();
  return contexts.filter((context) => {
    const key = `${context.cssPath}\0${context.variants.join(':')}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function htmlWarning(code, file, start, end, message) {
  return { code, file, start, end, message };
}

function isProjectInput(workspaceRoot, path) {
  return isWithin(workspaceRoot, path)
    && !relative(workspaceRoot, path).split(/[\\/]/).includes('node_modules');
}

async function snapshotFile(snapshots, path) {
  return recordSnapshot(snapshots, path, await readFile(path, 'utf8'));
}

function recordSnapshot(snapshots, path, source) {
  if (snapshots.has(path) && snapshots.get(path) !== source) {
    throw new Error(`Source changed during planning: ${path}`);
  }
  snapshots.set(path, source);
  return source;
}

function packageFailure(workspaceRoot, packageRoot, error) {
  const message = error instanceof Error ? error.message : String(error);
  return {
    package: normalizedRelativePath(workspaceRoot, packageRoot) || '.',
    message: message.startsWith(RECOVERABLE_INPUT_ERROR)
      ? message.slice(RECOVERABLE_INPUT_ERROR.length)
      : message,
  };
}

function isRecoverablePlanningError(error) {
  const message = error instanceof Error ? error.message : String(error);
  return message.startsWith(RECOVERABLE_INPUT_ERROR);
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

function normalizedRelativePath(root, path) {
  return relative(root, path).split(sep).join('/');
}

function stripCssComments(source) {
  return source.replace(/\/\*[\s\S]*?\*\//g, '');
}

function indexStylesheetDependents(styleSources) {
  const dependents = new Map();
  for (const [path, rawSource] of styleSources) {
    const source = stripCssComments(rawSource);
    const references = [
      ...source.matchAll(
        /(?:composes\s*:[^;{}]*?\bfrom\s+|@import\s+(?:url\(\s*)?)["']([^"']+)["']/g,
      ),
      ...source.matchAll(/@import\s+url\(\s*([^"'()\s]+)\s*\)/g),
    ];
    for (const target of new Set(references.map((match) => resolve(dirname(path), match[1])))) {
      if (target === path || !isStylesheetModule(target) || !styleSources.has(target)) continue;
      const paths = dependents.get(target) ?? [];
      paths.push(path);
      dependents.set(target, paths);
    }
  }
  for (const paths of dependents.values()) paths.sort();
  return dependents;
}

function resolveTailwindEntry(stylePaths, styleSources, configuredPath) {
  const entries = stylePaths.filter((path) => {
    if (extension(path) !== '.css') return false;
    const source = stripCssComments(styleSources.get(path));
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
