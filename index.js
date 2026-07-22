import { execFile } from 'node:child_process';
import { createRequire } from 'node:module';
import { chmod, lstat, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

import { parseHtmlSource } from './html.js';
import { planBatchMigration, validateCss } from './native.js';
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
      // Scan-only scripts are always retained as reference-only inputs: even
      // without a ".module." mention they can render components whose trees
      // the closed-world relationship proofs must see. Scan-only HTML matters
      // solely as a potential stylesheet consumer, and HTML entities can
      // encode any part of a linked filename, so retain ignored HTML
      // containing a link for parse5 to classify safely.
      const mayReferenceModule = extension(path) !== '.html' || /<link\b/i.test(source);
      if (!scope.targetable.has(path) && !mayReferenceModule) return undefined;
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
    rules: rules.map((rule) => ({ ...rule, file: normalizedRelativePath(cwd, rule.file) })),
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
  for (const path of [explicitStyle, configuredEntry]) {
    if (path) await rejectSymlinkTarget(path, currentPackage);
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
      styleDependents,
    });
  } catch (error) {
    if (!options.force || isIntegrityError(error)) throw error;
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
    if (!options.force || isIntegrityError(error)) throw error;
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
    const compilerDependents = new Map();
    for (const stylePath of targets.sort()) {
      await rejectSymlinkTarget(stylePath, packageRoot);
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
        // Compile the snapshotted source, not the on-disk file: code loaded
        // during planning (e.g. Tailwind plugins) may have rewritten it since.
        compiled = await compileSassEntry(sass, stylePath, stylesheet.cssSource);
      } else if (extension(stylePath) === '.less') {
        less ??= await loadProjectLess(packageRoot);
        compiled = await compileLessEntry(less, stylePath, stylesheet.cssSource);
      }
      if (compiled) {
        validateCss(compiled.css);
        for (const loadedPath of compiled.loadedPaths) {
          if (!isProjectInput(workspaceRoot, loadedPath)) continue;
          const source = await snapshotFile(snapshots, loadedPath);
          if (!styleSources.has(loadedPath)) styleSources.set(loadedPath, source);
          if (loadedPath !== stylePath) {
            const dependents = compilerDependents.get(loadedPath) ?? [];
            dependents.push(stylePath);
            compilerDependents.set(loadedPath, dependents);
          }
        }
        stylesheet.analysisSource = compiled.css;
        stylesheet.sourceMappings = compiled.sourceMappings;
      }
      stylesheets.push(stylesheet);
    }
    for (const stylesheet of stylesheets) {
      const dependents = compilerDependents.get(stylesheet.cssPath) ?? [];
      if (dependents.length === 0) continue;
      stylesheet.isPartial = true;
      stylesheet.cssDependents = [...new Set([...stylesheet.cssDependents, ...dependents])].sort();
    }
  } catch (error) {
    if (!options.force || isIntegrityError(error)) throw error;
    return { failure: packageFailure(workspaceRoot, packageRoot, error) };
  }

  const request = {
    stylesheets,
    tailwindPath: tailwind.path,
    tailwindSource: tailwind.css,
    utilityPrefix: tailwind.designSystem.theme.prefix,
    themeTokens: tailwind.themeTokens,
    files,
  };
  let plan;
  try {
    plan = JSON.parse(planBatchMigration(JSON.stringify(request)));
  } catch (error) {
    if (!options.force || !isRecoverablePlanningError(error)) throw error;
    return { failure: packageFailure(workspaceRoot, packageRoot, error) };
  }

  try {
    plan = replanCompileFailures(tailwind, request, plan);
  } catch (error) {
    if (!options.force) throw error;
    return { failure: packageFailure(workspaceRoot, packageRoot, error) };
  }

  removeMigratedHtmlLinks(plan, preparedHtml);
  plan.warnings.push(...preparedHtml.warnings);
  for (const file of plan.files.filter((file) => extension(file.path) === '.html')) {
    parseHtmlSource(file.path, file.source);
  }
  for (const stylesheet of stylesheets.filter((stylesheet) => isPreprocessorPath(stylesheet.cssPath))) {
    const changed = plan.files.find((file) => file.path === stylesheet.cssPath);
    if (!changed && !plan.deletedFiles.includes(stylesheet.cssPath)) continue;
    const source = changed?.source ?? '';
    if (isSassPath(stylesheet.cssPath)) {
      validateCss((await compileSassEntry(sass, stylesheet.cssPath, source)).css);
    } else {
      validateCss((await compileLessEntry(less, stylesheet.cssPath, source)).css);
    }
  }
  return { plan };
}

// A candidate Tailwind refuses to compile retains its owning rule(s) instead
// of aborting the run: block those rules and replan until every applied
// candidate compiles. Each iteration blocks at least one new rule, so the
// loop is bounded by the rule count; if a failing candidate cannot be
// attributed to a new rule, fall back to the package-level failure path.
function replanCompileFailures(tailwind, request, initialPlan) {
  let plan = initialPlan;
  const blockedByStylesheet = new Map();
  const maxIterations = plan.rules.length + 1;
  for (let iteration = 0; ; iteration += 1) {
    const failing = invalidCandidates(tailwind, plan.candidates);
    if (failing.length === 0) break;
    let progressed = false;
    for (const rule of plan.rules) {
      const failed = rule.candidates.filter((candidate) => failing.includes(candidate));
      if (failed.length === 0) continue;
      let blocked = blockedByStylesheet.get(rule.file);
      if (!blocked) blockedByStylesheet.set(rule.file, blocked = new Map());
      const key = `${rule.ruleId.start}-${rule.ruleId.end}`;
      let entry = blocked.get(key);
      if (!entry) {
        blocked.set(key, entry = { ruleId: rule.ruleId, candidates: new Set() });
        progressed = true;
      }
      for (const candidate of failed) entry.candidates.add(candidate);
    }
    if (!progressed || iteration >= maxIterations) {
      throw new Error(`Tailwind did not generate CSS for candidate: ${failing[0]}`);
    }
    plan = JSON.parse(planBatchMigration(JSON.stringify({
      ...request,
      stylesheets: request.stylesheets.map((stylesheet) => ({
        ...stylesheet,
        blockedRules: [...(blockedByStylesheet.get(stylesheet.cssPath)?.values() ?? [])]
          .map((entry) => entry.ruleId),
      })),
    })));
  }
  for (const [cssPath, blocked] of blockedByStylesheet) {
    for (const { ruleId, candidates } of blocked.values()) {
      const failed = [...candidates].sort().map((candidate) => `\`${candidate}\``).join(', ');
      plan.warnings.push({
        code: 'candidate-compilation-failure',
        file: cssPath,
        start: ruleId.start,
        end: ruleId.end,
        message: `Tailwind did not generate CSS for ${failed}, so the rule is retained.`,
      });
    }
  }
  return plan;
}

function removeMigratedHtmlLinks(plan, preparedHtml) {
  const unlinked = new Set(plan.unlinkedFiles);
  const linksByFile = new Map();
  for (const link of preparedHtml.removableLinks) {
    if (!unlinked.has(link.cssPath)) continue;
    const links = linksByFile.get(link.filePath) ?? new Set();
    links.add(`${link.href}\0${link.media}`);
    linksByFile.set(link.filePath, links);
  }

  for (const [filePath, removable] of linksByFile) {
    const planned = plan.files.find((file) => file.path === filePath);
    const original = preparedHtml.files.find((file) => file.path === filePath);
    if (!original) continue;
    const source = planned?.source ?? original.source;
    const links = parseHtmlSource(filePath, source).links
      .filter((link) => removable.has(`${link.href}\0${link.media}`))
      .sort((left, right) => right.tagStart - left.tagStart);
    let bytes = Buffer.from(source);
    for (const link of links) {
      bytes = Buffer.concat([bytes.subarray(0, link.tagStart), bytes.subarray(link.tagEnd)]);
    }
    const updated = bytes.toString();
    if (updated === source) continue;
    if (planned) planned.source = updated;
    else plan.files.push({ path: filePath, source: updated });
  }
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
  styleDependents,
}) {
  const files = [];
  const stylePaths = new Set();
  const generatedPaths = new Set();
  const removableLinks = [];
  const warnings = [];

  const htmlFiles = sourceFiles.filter((file) => extension(file.path) === '.html');
  // Package-owned HTML goes first so stylesheets it discovers are claimed for
  // this package before foreign consumers are matched against that ownership.
  const orderedFiles = [
    ...htmlFiles.filter((file) => pathOwners.get(file.path) === packageRoot),
    ...htmlFiles.filter((file) => pathOwners.get(file.path) !== packageRoot),
  ];
  for (const file of orderedFiles) {
    const owner = pathOwners.get(file.path);
    // Foreign HTML is analyzed only as a consumer of this package's
    // stylesheets; its other links and attributes are its own package's
    // concern, so they never warn here.
    const foreign = owner !== packageRoot;
    if (foreign && !owner) continue;
    const referenceRoot = foreign ? owner : packageRoot;
    const parsed = parseHtmlSource(file.path, file.source);
    const contexts = [];
    let linkBase = dirname(file.path);
    const base = parsed.bases[0];
    if (base) {
      const baseReference = base.href.split(/[?#]/, 1)[0];
      const basePath = base.writable && (baseReference === ''
        ? file.path
        : localHtmlReference(referenceRoot, dirname(file.path), base.href));
      if (!basePath || !isWithin(referenceRoot, basePath)) {
        if (!foreign) {
          warnings.push(htmlWarning(
            'unsupported-html-base',
            file.path,
            base.start,
            base.end,
            'A remote or unrepresentable base URL prevents safe stylesheet link resolution.',
          ));
        }
        linkBase = undefined;
      } else {
        linkBase = base.href.split(/[?#]/, 1)[0].endsWith('/') ? basePath : dirname(basePath);
      }
    }
    for (const link of parsed.links) {
      const linkedPath = linkBase && localHtmlReference(referenceRoot, linkBase, link.href);
      if (!linkedPath || !isWithin(packageRoot, linkedPath)
        || (foreign && pathOwners.get(linkedPath) !== packageRoot)) {
        if (!foreign) {
          warnings.push(htmlWarning(
            'unsupported-html-stylesheet-link',
            file.path,
            link.start,
            link.end,
            'Only local package stylesheet links are analyzed.',
          ));
        }
        continue;
      }
      const variants = mediaVariants(link.media);
      if (variants === undefined) {
        const cssPath = (foreign ? undefined : inferredPreprocessorPath({
          path: linkedPath,
          packageRoot,
          styleSources,
          pathOwners,
          styleDependents,
        })) ?? linkedPath;
        contexts.push({ cssPath, variants: [], direct: true, analyzable: false });
        if (!foreign) {
          warnings.push(htmlWarning(
            'unsupported-link-media',
            file.path,
            link.start,
            link.end,
            `The stylesheet link media condition ${JSON.stringify(link.media)} cannot be represented safely.`,
          ));
        }
        continue;
      }
      const contextStart = contexts.length;
      await collectHtmlStyleContexts({
        path: linkedPath,
        variants,
        direct: true,
        packageRoot,
        sourceFiles,
        styleSources,
        snapshots,
        pathOwners,
        styleDependents,
        stylePaths,
        generatedPaths,
        contexts,
        warnings,
        visited: new Set(),
      });
      const directContext = contexts[contextStart];
      if (directContext?.direct) {
        removableLinks.push({
          filePath: file.path,
          cssPath: directContext.cssPath,
          href: link.href,
          media: link.media,
        });
      }
    }

    if (foreign && contexts.length === 0) continue;
    if (!foreign && contexts.length > 0) {
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
      htmlReferencesSafe: parsed.dynamicAttributes.length === 0,
      htmlScriptText: parsed.scriptText,
    });
  }

  return { files, stylePaths, generatedPaths, removableLinks, warnings };
}

async function collectHtmlStyleContexts(state) {
  const key = `${state.path}\0${state.variants.join(':')}`;
  if (state.visited.has(key)) return;
  state.visited.add(key);
  if (!isStylesheetPath(state.path)) return;
  const owner = state.pathOwners.get(state.path);
  if (owner && owner !== state.packageRoot) {
    state.warnings.push(htmlWarning(
      'cross-package-stylesheet-link',
      state.path,
      0,
      0,
      'A stylesheet owned by another package is not analyzed outside workspace mode.',
    ));
    return;
  }

  let source;
  try {
    source = state.styleSources.get(state.path)
      ?? await snapshotFile(state.snapshots, state.path);
  } catch (error) {
    if (error.code === 'ENOENT') {
      if (extension(state.path) === '.css') addInferredPreprocessorContext(state);
      return;
    }
    throw error;
  }
  if (!state.styleSources.has(state.path)) state.styleSources.set(state.path, source);
  if (!owner) state.pathOwners.set(state.path, state.packageRoot);

  if (extension(state.path) === '.css' && addInferredPreprocessorContext(state)) return;

  state.stylePaths.add(state.path);
  state.contexts.push({
    cssPath: state.path,
    variants: state.variants,
    direct: state.direct,
    analyzable: true,
  });
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
    // Link-discovered stylesheets never went through indexStylesheetDependents,
    // so record their import edges here or deletion could leave this importer
    // pointing at a removed module.
    if (importedPath !== state.path
      && (isStylesheetModule(importedPath) || isPreprocessorPath(importedPath))) {
      addStyleDependent(state.styleDependents, importedPath, state.path);
    }
    await collectHtmlStyleContexts({
      ...state,
      path: importedPath,
      // Deduplicate so a cyclic import chain cannot grow the variant list and
      // mint a fresh visited key on every lap.
      variants: [...new Set([...state.variants, ...variants])],
      direct: false,
    });
  }
}

function inferredPreprocessorPath(state) {
  const stem = basename(state.path, '.css');
  const matches = [...state.styleSources.keys()].filter((path) =>
    isPreprocessorPath(path)
      && state.pathOwners.get(path) === state.packageRoot
      && !basename(path).startsWith('_')
      && !state.styleDependents.has(path)
      && basename(path, extension(path)) === stem,
  );
  return matches.length === 1 ? matches[0] : undefined;
}

function addInferredPreprocessorContext(state) {
  // A source importing the generated CSS pins the artifact itself: excluding
  // it from planning while migrating the inferred entry could delete the only
  // source able to rebuild the file that import depends on.
  if (state.styleSources.has(state.path)
    && state.sourceFiles.some((file) =>
      extension(file.path) !== '.html' && sourceReferencesStyle(file, state.path))) {
    return false;
  }
  const path = inferredPreprocessorPath(state);
  if (!path) return false;
  state.generatedPaths.add(state.path);
  state.stylePaths.add(path);
  state.contexts.push({
    cssPath: path,
    variants: state.variants,
    direct: state.direct,
    analyzable: true,
  });
  state.warnings.push(htmlWarning(
    'inferred-preprocessor-source',
    state.path,
    0,
    0,
    `The linked CSS was matched to the unique preprocessor filename ${basename(path)}.`,
  ));
  return true;
}

function cssImports(source) {
  const imports = [];
  const masked = maskCssComments(source);
  let depth = 0;
  let quote;
  // Browsers ignore @import once any block rule has appeared, so imports
  // after the first top-level `{` never load and must not be traversed.
  let importsAllowed = true;
  for (let index = 0; index < masked.length; index += 1) {
    const character = masked[index];
    if (quote) {
      if (character === '\\') index += 1;
      else if (character === quote) quote = undefined;
      continue;
    }
    if (character === '"' || character === "'") {
      quote = character;
      continue;
    }
    if (character === '{') {
      depth += 1;
      importsAllowed = false;
      continue;
    }
    if (character === '}') {
      depth = Math.max(0, depth - 1);
      continue;
    }
    if (!importsAllowed || depth !== 0
      || masked.slice(index, index + 7).toLowerCase() !== '@import'
      || /[-\w]/.test(masked[index + 7] ?? '')) continue;

    let end = index + 7;
    let importQuote;
    let parentheses = 0;
    for (; end < masked.length; end += 1) {
      const next = masked[end];
      if (importQuote) {
        if (next === '\\') end += 1;
        else if (next === importQuote) importQuote = undefined;
      } else if (next === '"' || next === "'") importQuote = next;
      else if (next === '(') parentheses += 1;
      else if (next === ')') parentheses = Math.max(0, parentheses - 1);
      else if (next === ';' && parentheses === 0) break;
    }
    if (end >= masked.length) continue;
    const statement = masked.slice(index, end + 1);
    const match = /^@import\s+(?:url\(\s*)?(?:["']([^"']+)["']|([^"'()\s;]+))\s*\)?\s*([^;]*);$/i.exec(statement);
    if (match) imports.push({
      href: match[1] ?? match[2],
      media: match[3].trim(),
      start: utf8Offset(source, index),
      end: utf8Offset(source, end + 1),
    });
    index = end;
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
  const unique = new Map();
  for (const context of contexts) {
    const key = `${context.cssPath}\0${context.variants.join(':')}\0${context.analyzable}`;
    const existing = unique.get(key);
    if (existing) existing.direct ||= context.direct;
    else unique.set(key, { ...context });
  }
  return [...unique.values()];
}

function htmlWarning(code, file, start, end, message) {
  return { code, file, start, end, message };
}

function isProjectInput(workspaceRoot, path) {
  return isWithin(workspaceRoot, path)
    && !relative(workspaceRoot, path).split(/[\\/]/).includes('node_modules');
}

async function rejectSymlinkTarget(path, root) {
  for (let current = path; isWithin(root, current); current = dirname(current)) {
    if ((await lstat(current)).isSymbolicLink()) {
      throw new Error(`Refusing to migrate a symbolic-link target: ${path}`);
    }
    if (current === root) break;
  }
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

function isIntegrityError(error) {
  const message = error instanceof Error ? error.message : String(error);
  return message.startsWith('Source changed during planning:');
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

function maskCssComments(source) {
  return source.replace(/\/\*[\s\S]*?\*\//g, (comment) => comment.replace(/[^\r\n]/g, ' '));
}

function stripCssComments(source) {
  return maskCssComments(source);
}

function utf8Offset(source, index) {
  return Buffer.byteLength(source.slice(0, index));
}

function indexStylesheetDependents(styleSources) {
  const dependents = new Map();
  for (const [path, rawSource] of styleSources) {
    const source = stripCssComments(rawSource);
    const references = [
      ...[...source.matchAll(/composes\s*:[^;{}]*?\bfrom\s+["']([^"']+)["']/g)]
        .map((match) => match[1]),
      ...[...source.matchAll(/@(?:use|forward)\s+["']([^"']+)["']/g)]
        .map((match) => match[1]),
      ...[...source.matchAll(/@import\s+(?:\([^)]*\)\s*)?(?:([^;{}]+);|([^;{}\r\n]+))/g)]
        .flatMap((statement) => [...(statement[1] ?? statement[2]).matchAll(/["']([^"']+)["']/g)]
          .map((match) => match[1])),
      ...[...source.matchAll(/@import\s+url\(\s*([^"'()\s]+)\s*\)/g)]
        .map((match) => match[1]),
    ];
    for (const reference of new Set(references)) {
      for (const target of stylesheetReferenceTargets(path, reference, styleSources)) {
        if (target === path || (!isStylesheetModule(target) && !isPreprocessorPath(target))) continue;
        const paths = dependents.get(target) ?? [];
        paths.push(path);
        dependents.set(target, paths);
      }
    }
  }
  for (const [target, paths] of dependents) dependents.set(target, [...new Set(paths)].sort());
  return dependents;
}

function addStyleDependent(styleDependents, target, importer) {
  const paths = styleDependents.get(target) ?? [];
  if (paths.includes(importer)) return;
  paths.push(importer);
  paths.sort();
  styleDependents.set(target, paths);
}

function stylesheetReferenceTargets(importer, reference, styleSources) {
  const target = resolve(dirname(importer), reference);
  const candidates = STYLESHEET_SYNTAX.has(extension(target))
    ? [target]
    : [...STYLESHEET_SYNTAX.keys()].flatMap((syntax) => [
      `${target}${syntax}`,
      join(dirname(target), `_${basename(target)}${syntax}`),
      join(target, `_index${syntax}`),
      join(target, `index${syntax}`),
    ]);
  return candidates.filter((path) => styleSources.has(path));
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

function invalidCandidates(tailwind, candidates) {
  const generated = tailwind.designSystem.candidatesToCss(candidates);
  return candidates.filter((_, index) => generated[index] === null);
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
