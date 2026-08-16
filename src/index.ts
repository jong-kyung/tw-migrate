import { lstat, readFile } from "node:fs/promises";
import { basename, dirname, extname, join, resolve } from "node:path";

import { unifiedDiff } from "./util/diff.ts";
import { collectFiles, resolveScope, scannerIgnoredPaths } from "./discovery.ts";
import { parseHtmlSource, TEMPLATE_EXPRESSIONS, TEMPLATE_MARKERS } from "./parser/html.ts";
import { localHtmlReference, preparePackageHtml } from "./plan/html.ts";
import { planBatchMigration, sourceAnalysis, stylesheetAnalysis, validateCss } from "./native.ts";
import {
  indexStylesheetDependents,
  isIntegrityError,
  isProjectInput,
  isMissingStyleCompilerError,
  isRecoverablePlanningError,
  isStylesheetModule,
  isStylesheetPath,
  isWithin,
  normalizedRelativePath,
  packageFailure,
  recordSnapshot,
  rejectSymlinkTarget,
  snapshotFile,
  snapshotLoadedSource,
  stylesheetSyntax,
  SOURCE_EXTENSIONS,
} from "./util/shared.ts";
import { compileStyleEntry, isPreprocessorPath, isSassPath } from "./parser/style-compiler.ts";
import type { StyleCompilers } from "./parser/style-compiler.ts";
import {
  findTailwindEntries,
  invalidCandidates,
  loadTailwind,
  selectTailwindEntry,
} from "./tailwind.ts";
import {
  acceptedCandidateAliases,
  scanStylesheetReservations,
  spellingReservations,
} from "./plan/canonicalize.ts";
import { appendFontTheme, registerFontTokens } from "./plan/fonts.ts";
import { utilitySegment } from "./plan/canonicalize.ts";
import { importsStylesheet, proveSharedEntry, tailwindEntryCatalog } from "./plan/entry.ts";
import type { SharedEntryProofs } from "./plan/entry.ts";
import {
  appendMediaDefinitions,
  collectMemberMediaConditions,
  planMediaExtraction,
  probeCandidate,
  usedGeneratedDefinitions,
} from "./plan/media.ts";
import type { MediaCollection, MediaExtraction } from "./plan/media.ts";
import { createRequire } from "node:module";

import { verifyVueSource } from "./parser/vue.ts";
import type { VueStyleRange } from "./parser/vue.ts";
import { preparePackageVue, vueWarningsOnlyResult } from "./plan/vue.ts";
import { verifySnapshots, writeChanges } from "./util/write.ts";
import type {
  DesignSystem,
  HtmlContext,
  LoadedTailwind,
  PreparedVue,
  MigrateOptions,
  MigrationContext,
  MigrationFailure,
  MigrationReport,
  MigrationWarning,
  Plan,
  PlannedFile,
  PlannerRequest,
  PlanResult,
  PlanRule,
  PreparedHtml,
  PreparedSourceFile,
  RuleSpan,
  SourceFile,
  StylesheetEntry,
} from "./types.ts";

export type {
  MigrateOptions,
  MigrationFailure,
  MigrationReport,
  MigrationWarning,
  RuleReport,
} from "./types.ts";

function indexWarningPositions(
  source: string,
  offsets: Set<number>,
  unicodeSeparatorsAreNewlines: boolean,
  formFeedIsNewline: boolean,
  initial = { line: 1, column: 1 },
) {
  const targets = [...offsets].sort((left, right) => left - right);
  const positions = new Map<number, { line: number; column: number }>();
  let target = 0;
  let byte = 0;
  let { line, column } = initial;
  let previousWasCarriageReturn = false;
  const record = (): void => {
    while (targets[target] === byte) {
      positions.set(byte, { line, column });
      target += 1;
    }
  };

  record();
  for (const character of source) {
    byte += Buffer.byteLength(character);
    while ((targets[target] ?? Infinity) < byte) target += 1;
    const newline =
      character === "\r" ||
      (character === "\n" && !previousWasCarriageReturn) ||
      (unicodeSeparatorsAreNewlines && (character === "\u2028" || character === "\u2029")) ||
      (formFeedIsNewline && character === "\f");
    if (newline) {
      line += 1;
      column = 1;
    } else if (character !== "\n") {
      column += character.length;
    }
    previousWasCarriageReturn = character === "\r";
    record();
  }
  return positions;
}

export async function migrate(options: MigrateOptions = {}): Promise<MigrationReport> {
  if (options.styleFile && options.workspaces) {
    throw new TypeError("styleFile cannot be combined with workspaces");
  }
  if (options.tailwindCss && options.workspaces) {
    throw new TypeError("tailwindCss cannot be combined with workspaces");
  }

  const scope = await resolveScope(options);
  const { cwd, workspaceRoot, selectedPackages, explicitStyle, configuredEntry } = scope;

  // A prior interrupted run's scope is unknowable from the current flags
  // (it may have covered other packages), so scan the whole workspace root.
  const leftovers = await collectFiles(workspaceRoot, (path) =>
    basename(path).includes(".tw-migrate-"),
  );
  if (leftovers.length > 0) {
    const listed = leftovers
      .sort()
      .map((path) => `  ${normalizedRelativePath(cwd, path)}`)
      .join("\n");
    throw new Error(
      `Found leftover tw-migrate files from an interrupted run:\n${listed}\n` +
        'Restore each ".<name>.tw-migrate-backup-*" file by renaming it back to "<name>", ' +
        'delete any remaining ".<name>.tw-migrate-*" staging files, then re-run.',
    );
  }

  const snapshots = new Map<string, string>();
  const stylePaths = scope.scannedPaths.filter(isStylesheetPath);
  const sourcePaths = scope.scannedPaths.filter((path) => SOURCE_EXTENSIONS.has(extname(path)));
  const [styleSources, sourceCandidates] = await Promise.all([
    Promise.all(
      stylePaths.map(
        async (path): Promise<[string, string]> => [path, await snapshotFile(snapshots, path)],
      ),
    ).then((entries) => new Map(entries)),
    Promise.all(
      sourcePaths.map(async (path) => {
        const source = await readFile(path, "utf8");
        // Scan-only scripts are always retained as reference-only inputs: even
        // without a ".module." mention they can render components whose trees
        // the closed-world relationship proofs must see. Scan-only HTML matters
        // solely as a potential stylesheet consumer, and HTML entities can
        // encode any part of a linked filename, so retain ignored HTML
        // containing a link for parse5 to classify safely.
        const mayReferenceModule =
          extname(path) !== ".html" ||
          (() => {
            try {
              return parseHtmlSource(path, source).links.length > 0;
            } catch {
              return /<link\b/i.test(source);
            }
          })();
        if (!scope.targetable.has(path) && !mayReferenceModule) return undefined;
        return { path, source: recordSnapshot(snapshots, path, source) };
      }),
    ),
    ...selectedPackages.map((packageRoot) =>
      snapshotFile(snapshots, join(packageRoot, "package.json")),
    ),
  ]);
  const sourceFiles = sourceCandidates.flatMap((file) => (file ? [file] : []));
  // An explicit .vue selection is a source file, not a stylesheet input; only
  // real stylesheets may enter the stylesheet maps.
  if (explicitStyle && isStylesheetPath(explicitStyle) && !styleSources.has(explicitStyle)) {
    styleSources.set(explicitStyle, await snapshotFile(snapshots, explicitStyle));
  }
  if (configuredEntry && !styleSources.has(configuredEntry)) {
    styleSources.set(configuredEntry, await snapshotFile(snapshots, configuredEntry));
  }

  const vueStyleRanges = new Map<string, VueStyleRange[]>();
  const context: MigrationContext = {
    ...scope,
    options,
    snapshots,
    styleSources,
    sourceFiles,
    styleDependents: indexStylesheetDependents(styleSources),
    vueStyleRanges,
    entryCatalog: tailwindEntryCatalog(styleSources, scope.pathOwners),
    // Discovery keeps Git-ignored consumers for deletion safety, but the
    // Tailwind scanner skips them in every mode, so ignore detection must
    // not depend on --workspaces.
    ignoredPaths: await scannerIgnoredPaths(
      workspaceRoot,
      sourceFiles.map((file) => file.path),
    ),
  };
  const failures: MigrationFailure[] = [];
  const plans: Plan[] = [];
  // Preparation completes for every package before any planning so that
  // packages resolving to one shared Tailwind entry can plan as a group
  // with one name allocation and one composed entry edit.
  const prepared: PreparedPackage[] = [];
  for (const packageRoot of selectedPackages) {
    const preparation = await preparePackage(context, packageRoot);
    if (preparation.failure) failures.push(preparation.failure);
    else if (preparation.plan) plans.push(preparation.plan);
    if (preparation.prepared) prepared.push(preparation.prepared);
  }
  const groups = new Map<string, PreparedPackage[]>();
  for (const preparation of prepared) {
    const members = groups.get(preparation.tailwind.path) ?? [];
    members.push(preparation);
    groups.set(preparation.tailwind.path, members);
  }
  for (const [, members] of [...groups].sort(([left], [right]) =>
    left < right ? -1 : left > right ? 1 : 0,
  )) {
    for (const result of await planPreparedGroup(context, members)) {
      if (result.failure) failures.push(result.failure);
      else if (result.plan) plans.push(result.plan);
    }
  }

  const originals = new Map<string, string>([
    ...styleSources,
    ...sourceFiles.map((file): [string, string] => [file.path, file.source]),
  ]);
  const { filesByPath, deletedPaths, candidates, rules, warnings, convertedRules, retainedRules } =
    mergePlans(plans, originals);

  const changed = [...filesByPath.values()]
    .map((file) => ({ ...file, before: originals.get(file.path) ?? "" }))
    .filter((file) => file.before !== file.source);
  const deleted = [...deletedPaths].map((path) => ({ path, before: originals.get(path) ?? "" }));
  const operations: { path: string; before: string; source?: string }[] = [
    ...changed,
    ...deleted,
  ].sort((left, right) => left.path.localeCompare(right.path));
  const changedFiles = operations.map((file) => normalizedRelativePath(cwd, file.path));
  const diff = operations
    .map((file) =>
      unifiedDiff(normalizedRelativePath(cwd, file.path), file.before, file.source ?? ""),
    )
    .join("");

  if (options.write && operations.length > 0) {
    await verifySnapshots(snapshots);
    await writeChanges(changed, deleted);
  }

  warnings.sort(
    (left, right) =>
      left.file.localeCompare(right.file) ||
      left.start - right.start ||
      left.end - right.end ||
      left.code.localeCompare(right.code),
  );
  const warningOffsets = new Map<string, Set<number>>();
  for (const warning of warnings) {
    if (warning.start === 0 && warning.end === 0) continue;
    const offsets = warningOffsets.get(warning.file) ?? new Set<number>();
    offsets.add(warning.start).add(warning.end);
    warningOffsets.set(warning.file, offsets);
  }
  const warningPositions = new Map(
    [...warningOffsets].flatMap(([file, offsets]) => {
      const source = snapshots.get(file);
      if (!source) return [];
      const extension = extname(file);
      const javascriptSource =
        SOURCE_EXTENSIONS.has(extension) && extension !== ".html" && extension !== ".vue";
      const styleRanges = vueStyleRanges.get(file) ?? [];
      const defaultOffsets = new Set(offsets);
      for (const range of styleRanges) defaultOffsets.add(range.start);
      const defaultPositions = indexWarningPositions(
        source,
        defaultOffsets,
        javascriptSource,
        isStylesheetPath(file),
      );
      const bytes = Buffer.from(source);
      const sortedOffsets = [...offsets].sort((left, right) => left - right);
      const stylePositions = new Map<number, { line: number; column: number }>();
      let offsetIndex = 0;
      for (const range of styleRanges) {
        while ((sortedOffsets[offsetIndex] ?? Infinity) < range.start) offsetIndex += 1;
        const relativeOffsets = new Set<number>();
        while ((sortedOffsets[offsetIndex] ?? Infinity) <= range.end) {
          relativeOffsets.add(sortedOffsets[offsetIndex] - range.start);
          offsetIndex += 1;
        }
        const initial = defaultPositions.get(range.start);
        if (!initial) continue;
        for (const [offset, position] of indexWarningPositions(
          bytes.subarray(range.start, range.end).toString(),
          relativeOffsets,
          false,
          true,
          initial,
        )) {
          stylePositions.set(range.start + offset, position);
        }
      }
      return [[file, { default: defaultPositions, style: stylePositions }] as const];
    }),
  );
  failures.sort((left, right) => left.package.localeCompare(right.package));
  return {
    changedFiles,
    diff,
    convertedRules,
    retainedRules,
    // `stylesheet` is internal compile-failure attribution, not public shape.
    rules: rules.map(({ stylesheet: _, ...rule }) => ({
      ...rule,
      file: normalizedRelativePath(cwd, rule.file),
    })),
    candidates: [...candidates].sort(),
    warnings: warnings.map((warning) => {
      const indexed = warningPositions.get(warning.file);
      const styleFrom = indexed?.style.get(warning.start);
      const styleTo = indexed?.style.get(warning.end);
      const located = warning.start !== 0 || warning.end !== 0;
      const from = located ? (styleFrom ?? indexed?.default.get(warning.start)) : undefined;
      const to = located
        ? styleFrom && styleTo
          ? styleTo
          : indexed?.default.get(warning.end)
        : undefined;
      return {
        ...warning,
        ...(from && to
          ? { line: from.line, column: from.column, endLine: to.line, endColumn: to.column }
          : {}),
        file: normalizedRelativePath(cwd, warning.file),
      };
    }),
    failures,
  };
}

// Preparation and planning are separate phases so a shared-entry group can
// finish preparing every member package before any consumer is planned.
interface PreparedPackage {
  packageRoot: string;
  preparedHtml: PreparedHtml;
  preparedVue: PreparedVue;
  styleCompilers: StyleCompilers;
  files: PlannedFile[];
  stylesheets: StylesheetEntry[];
  tailwind: LoadedTailwind;
  /// The dual proofs of an ancestor-shared entry; absent for a package
  /// that owns its entry, whose runtime loads it by definition.
  sharedProofs?: SharedEntryProofs;
  /// Preparation-time warnings, such as unproven shared-entry flows.
  warnings: MigrationWarning[];
}

// When `prepared` is absent the result is final: a recoverable package
// failure or a warnings-only plan from preparation.
type PackagePreparation = PlanResult & { prepared?: PreparedPackage };

async function preparePackage(
  context: MigrationContext,
  packageRoot: string,
): Promise<PackagePreparation> {
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
    selectedPackages,
    scannedPaths,
    vueStyleRanges,
  } = context;
  const recover = (error: unknown, fatal = false): PlanResult => {
    if (!options.force || fatal) throw error;
    return { failure: packageFailure(workspaceRoot, packageRoot, error) };
  };
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
    return recover(error, isIntegrityError(error));
  }
  const styleCompilers: StyleCompilers = {};
  let preparedVue;
  try {
    preparedVue = await preparePackageVue({
      packageRoot,
      styleCompilers,
      sourceFiles,
      styleSources,
      pathOwners,
      targetable,
      explicitStyle,
      snapshots,
      workspaceRoot,
      workspaces: options.workspaces,
      scannedPaths,
    });
  } catch (error) {
    return recover(error, isIntegrityError(error));
  }
  for (const [path, preparedRanges] of preparedVue.styleRanges) {
    const ranges = vueStyleRanges.get(path) ?? [];
    ranges.push(...preparedRanges);
    ranges.sort((left, right) => left.start - right.start);
    vueStyleRanges.set(path, ranges);
  }
  // An explicit .vue selection that produced nothing to plan must surface
  // its retention warnings without requiring an unrelated Tailwind entry.
  if (
    explicitStyle &&
    extname(explicitStyle) === ".vue" &&
    preparedVue.stylesheets.length === 0 &&
    ![...preparedVue.stylePaths].some((path) => !isStylesheetModule(path))
  ) {
    return vueWarningsOnlyResult(preparedVue);
  }
  const packageSources: PreparedSourceFile[] = [
    ...sourceFiles
      .filter((file) => extname(file.path) !== ".html")
      .map((file) => preparedVue.files.get(file.path) ?? file),
    ...preparedHtml.files,
  ];
  const ownedStyles = [...styleSources.keys()].filter(
    (path) =>
      pathOwners.get(path) === packageRoot &&
      (targetable.has(path) ||
        preparedHtml.stylePaths.has(path) ||
        preparedVue.stylePaths.has(path)),
  );
  if (ownedStyles.length === 0 && preparedVue.stylesheets.length === 0) {
    return vueWarningsOnlyResult(preparedVue);
  }

  let tailwindPath: string | undefined;
  let tailwindEntries: string[];
  let entryOwner = packageRoot;
  let sharedProofs: SharedEntryProofs | undefined;
  const ownEntries = findTailwindEntries(ownedStyles, styleSources);
  // Ancestor-shared resolution stays scoped to extraction until part 4
  // flips the default together with the packaged snapshots: without the
  // option, workspace behavior is byte-identical to today.
  if (
    ownEntries.length === 0 &&
    configuredEntry === undefined &&
    options.workspaces &&
    options.extractMediaQueries !== false
  ) {
    // Ancestry identifies candidates only; selecting an ancestor entry
    // requires the dual loading and scan proofs, level by level from the
    // nearest ancestor package.
    let packageJson: Record<string, unknown> = {};
    try {
      const rawPackageJson = snapshots.get(join(packageRoot, "package.json"));
      if (rawPackageJson !== undefined) {
        const parsed: unknown = JSON.parse(rawPackageJson);
        if (parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)) {
          packageJson = { ...parsed };
        }
      }
    } catch (error) {
      return recover(error);
    }
    const ancestors = selectedPackages
      .filter((root) => root !== packageRoot && isWithin(root, packageRoot))
      .sort((left, right) => right.length - left.length);
    for (const ancestor of ancestors) {
      const proven: { entry: string; proofs: SharedEntryProofs }[] = [];
      for (const candidate of context.entryCatalog.get(ancestor) ?? []) {
        const proofs = proveSharedEntry({
          packageRoot,
          entry: candidate,
          entrySource: styleSources.get(candidate) ?? "",
          styleSources,
          packageSources,
          owned: (path) => pathOwners.get(path) === packageRoot,
          writable: (path) => targetable.has(path) && pathOwners.get(path) === packageRoot,
          packageJson,
          ignoredPaths: context.ignoredPaths,
        });
        if (proofs) proven.push({ entry: candidate, proofs });
      }
      if (proven.length > 1) {
        return recover(
          new Error("Multiple proven ancestor Tailwind entries were found. Pass --tailwind-css."),
        );
      }
      if (proven.length === 1) {
        [{ entry: tailwindPath, proofs: sharedProofs }] = proven;
        entryOwner = ancestor;
        break;
      }
    }
  }
  if (sharedProofs && tailwindPath !== undefined) {
    tailwindEntries = [];
  } else {
    tailwindEntries = ownEntries;
    try {
      tailwindPath = selectTailwindEntry(ownEntries, configuredEntry);
    } catch (error) {
      return recover(error);
    }
  }

  const excludedEntries = new Set([...tailwindEntries, tailwindPath]);
  const explicitCss =
    explicitStyle && extname(explicitStyle) !== ".vue" ? explicitStyle : undefined;
  const targets = explicitCss
    ? [explicitCss]
    : explicitStyle
      ? [...preparedVue.stylePaths].filter(
          (path) => !isStylesheetModule(path) && !excludedEntries.has(path),
        )
      : ownedStyles.filter(
          (path) =>
            !excludedEntries.has(path) &&
            !preparedHtml.generatedPaths.has(path) &&
            (!isPreprocessorPath(path) ||
              preparedHtml.stylePaths.has(path) ||
              preparedVue.stylePaths.has(path) ||
              packageSources.some((file) => importsStylesheet(file, path))),
        );
  if (targets.length === 0 && preparedVue.stylesheets.length === 0) {
    return vueWarningsOnlyResult(preparedVue);
  }
  if (targets.some((path) => excludedEntries.has(path))) {
    throw new Error("The Tailwind CSS entry cannot be migrated.");
  }
  // A stylesheet with any consumer outside the proven shared-entry flows
  // keeps its rules retained while the rest of the package migrates, and
  // every rejected target is reported instead of silently skipped.
  const proofWarnings: MigrationWarning[] = [];
  const unprovenFlow = (path: string): MigrationWarning => ({
    code: "unproven-shared-entry-flow",
    file: path,
    start: 0,
    end: 0,
    message:
      "A consumer flow could not be proven to load the shared Tailwind entry, so the stylesheet is retained.",
  });
  const provenTargets = sharedProofs
    ? targets.filter((path) => {
        const proven = sharedProofs.provenStyle(
          packageSources.filter(
            (file) =>
              importsStylesheet(file, path) ||
              (file.htmlStylesheets ?? []).some((linked) => linked.cssPath === path),
          ),
        );
        if (!proven) proofWarnings.push(unprovenFlow(path));
        return proven;
      })
    : targets;
  // Vue SFC styles carry the same proof obligation: the SFC itself is the
  // consumer its scoped and module rules ship with, so an SFC outside the
  // proven flows keeps its style entries retained.
  const provenVueStylesheets = sharedProofs
    ? preparedVue.stylesheets.filter((stylesheet) => {
        const proven = sharedProofs.provenStyle(
          packageSources.filter((file) => file.path === stylesheet.cssPath),
        );
        if (!proven && !proofWarnings.some((warning) => warning.file === stylesheet.cssPath)) {
          proofWarnings.push(unprovenFlow(stylesheet.cssPath));
        }
        return proven;
      })
    : preparedVue.stylesheets;
  if (provenTargets.length === 0 && provenVueStylesheets.length === 0) {
    return vueWarningsOnlyResult(preparedVue, proofWarnings);
  }

  let tailwind;
  try {
    // The entry owner supplies the project root for Tailwind, its imports,
    // plugins, and theme; the migrated package keeps supplying its own
    // stylesheets, consumers, and preprocessors.
    tailwind = await loadTailwind(entryOwner, tailwindPath, snapshots, workspaceRoot);
  } catch (error) {
    return recover(error, isIntegrityError(error));
  }

  const files = packageSources.map((file): PlannedFile => {
    const owner = pathOwners.get(file.path);
    return {
      ...file,
      writable:
        targetable.has(file.path) &&
        (options.workspaces
          ? owner !== undefined && selectedPackages.includes(owner)
          : owner === packageRoot),
    };
  });
  let stylesheets: StylesheetEntry[];
  try {
    stylesheets = [];
    const compilerDependents = new Map<string, string[]>();
    for (const stylePath of provenTargets.sort()) {
      await rejectSymlinkTarget(stylePath, packageRoot);
      const isPartial = isSassPath(stylePath) && basename(stylePath).startsWith("_");
      const stylesheet: StylesheetEntry = {
        cssPath: stylePath,
        cssSource: styleSources.get(stylePath) ?? "",
        // Workspace-relative so migrated keyframe scopes stay unique when
        // two packages share a module filename.
        cssModuleId: normalizedRelativePath(workspaceRoot, stylePath),
        cssDependents: styleDependents.get(stylePath) ?? [],
        syntax: stylesheetSyntax(stylePath),
        isModule: isStylesheetModule(stylePath),
        isPartial,
      };
      let compiled;
      if ((isSassPath(stylePath) && !isPartial) || extname(stylePath) === ".less") {
        // Compile the snapshotted source, not the on-disk file: code loaded
        // during planning (e.g. Tailwind plugins) may have rewritten it since.
        compiled = await compileStyleEntry(
          styleCompilers,
          packageRoot,
          stylePath,
          stylesheet.cssSource,
        );
      }
      if (compiled) {
        validateCss(compiled.css);
        for (const loadedPath of compiled.loadedPaths) {
          if (!isProjectInput(workspaceRoot, loadedPath)) continue;
          const source = await snapshotLoadedSource(snapshots, loadedPath);
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
      stylesheet.cssDependents = [
        ...new Set([...(stylesheet.cssDependents ?? []), ...dependents]),
      ].sort();
    }
  } catch (error) {
    return recover(error, isIntegrityError(error));
  }

  stylesheets.push(...provenVueStylesheets);

  return {
    prepared: {
      packageRoot,
      preparedHtml,
      preparedVue,
      styleCompilers,
      files,
      stylesheets,
      tailwind,
      sharedProofs,
      warnings: proofWarnings,
    },
  };
}

type BlockedRules = Map<
  number,
  Map<string, { ruleId: RuleSpan; authoredSpan: RuleSpan; candidates: Set<string> }>
>;

// Plan every package that resolved to one Tailwind entry as a group: one
// fixed media name allocation, one mutable composed entry source, and one
// final entry edit. A single-entry package is simply a group of one.
async function planPreparedGroup(
  context: MigrationContext,
  members: PreparedPackage[],
): Promise<PlanResult[]> {
  const { options, workspaceRoot, targetable } = context;
  const results: PlanResult[] = [];
  let active = [...members].sort((left, right) => {
    const leftPath = normalizedRelativePath(workspaceRoot, left.packageRoot);
    const rightPath = normalizedRelativePath(workspaceRoot, right.packageRoot);
    return leftPath < rightPath ? -1 : leftPath > rightPath ? 1 : 0;
  });
  const entry = active[0].tailwind;
  // Canonical spellings are reserved against every authored selector,
  // including the entry's loaded import graph, and a consumer page keeping
  // an opaque style source (a remote or unresolved stylesheet link, or an
  // inline style block) can contain any selector, so such a group keeps
  // its arbitrary spellings entirely.
  const reservations = spellingReservations(
    context.styleSources,
    entry.graphSources.map(
      (graphSource) => [`${graphSource.path}.graph.css`, graphSource.source] as const,
    ),
    new Set(
      entry.graphSources
        .map((graphSource) => graphSource.path)
        .filter((path) => path.includes("\0")),
    ),
  );
  const groupFiles = new Map(
    active.flatMap((member) => member.files.map((file) => [file.path, file] as const)),
  );
  const fileRoots = new Map(
    active.flatMap((member) =>
      member.files.map((file) => [file.path, member.packageRoot] as const),
    ),
  );
  const workspaceEntries = new Set([...context.entryCatalog.values()].flat());
  for (const file of groupFiles.values()) {
    if (reservations.unbounded) break;
    // Tailwind's automatic scanner never generated utilities for a
    // scanner-ignored source, so a canonical name may newly style its
    // static classes; the group keeps arbitrary spellings.
    if (context.ignoredPaths.has(file.path)) {
      reservations.unbounded = true;
      break;
    }
    const opaqueStylesheetLink = (link: string) => {
      // Root-relative hrefs resolve against the package root, matching
      // HTML preparation's own link resolution.
      const resolved = localHtmlReference(
        fileRoots.get(file.path) ?? context.workspaceRoot,
        dirname(file.path),
        link,
      );
      return (
        resolved === undefined ||
        !context.styleSources.has(resolved) ||
        (workspaceEntries.has(resolved) && resolved !== entry.path)
      );
    };
    const opaqueStylesheetImports = (records: { specifier: string; typeOnly: boolean }[]) => {
      for (const record of records) {
        if (record.typeOnly) continue;
        const specifier = record.specifier.split(/[?#]/, 1)[0];
        if (!isStylesheetPath(specifier)) {
          // A bare extensionless import can hide a package style entry
          // behind an exports map; whatever node cannot resolve (bundler
          // aliases, ESM-only maps) stays outside the reservation scope.
          if (extname(specifier) !== "" || specifier.startsWith(".")) continue;
          try {
            const resolved = createRequire(file.path).resolve(specifier);
            if (isStylesheetPath(resolved) && !context.styleSources.has(resolved)) {
              reservations.unbounded = true;
            }
          } catch {
            // Unresolvable specifiers are bundler-specific; see above.
          }
          continue;
        }
        const resolved = specifier.startsWith(".")
          ? resolve(dirname(file.path), specifier)
          : undefined;
        // A different workspace entry co-loads a design system this group
        // cannot validate against, matching the HTML-link rule below.
        if (
          resolved === undefined ||
          !context.styleSources.has(resolved) ||
          (workspaceEntries.has(resolved) && resolved !== entry.path)
        ) {
          reservations.unbounded = true;
        }
      }
    };
    if (extname(file.path) === ".html") {
      try {
        const parsed = parseHtmlSource(file.path, file.source);
        if (parsed.hasStyle) reservations.unbounded = true;
        // A <base> element changes what every relative link loads, so a
        // local file matching the raw href proves nothing about the sheet
        // the browser fetches.
        if (parsed.bases.length > 0) reservations.unbounded = true;
        // An unresolved local link loads a stylesheet the snapshot never
        // parsed, and a link to another package's Tailwind entry co-loads
        // a design system this group cannot validate against; full
        // cross-entry compile validation lands with font registration.
        for (const link of parsed.links) {
          if (opaqueStylesheetLink(link.href)) reservations.unbounded = true;
        }
        // These decoded values never reach Tailwind's raw-text scan: a
        // templated token's static lead reserves as a prefix, a token
        // with no static lead can be any spelling, and a complete
        // decoded token (an entity-bearing class) reserves its name.
        for (const value of parsed.dynamicClassValues) {
          const masked = value.replace(TEMPLATE_EXPRESSIONS, "\0");
          // A marker outside a balanced expression leaves the templated
          // region unknowable.
          if (TEMPLATE_MARKERS.test(masked)) {
            reservations.unbounded = true;
            continue;
          }
          for (const token of masked.split(/\s+/)) {
            if (token === "") continue;
            const index = token.indexOf("\0");
            if (index < 0) reservations.names.add(token);
            else if (index === 0) reservations.unbounded = true;
            else reservations.prefixes.add(token.slice(0, index));
          }
        }
        // The planner already treats inline scripts conservatively for
        // module retention; spelling reservations follow the same rule.
        // An analyzable script contributes its constrained prefixes and
        // stylesheet imports, and an unanalyzable one turns the group
        // opaque.
        if (parsed.scriptText.trim() !== "") {
          try {
            const script = sourceAnalysis(`${file.path}.inline.js`, parsed.scriptText);
            for (const prefix of script.templatePrefixes) reservations.prefixes.add(prefix);
            for (const property of script.customProperties) reservations.properties.add(property);
            if (script.customPropertiesUnbounded) reservations.propertiesUnbounded = true;
            opaqueStylesheetImports(script.imports);
          } catch {
            reservations.unbounded = true;
          }
        }
      } catch {
        reservations.unbounded = true;
      }
      continue;
    }
    // Statically constrained dynamic class construction reserves the
    // spellings its runtime values can complete, and a stylesheet loaded
    // through a source import outside the discovered snapshot (a package
    // or ignored sheet) can define any selector, so it turns the group
    // opaque.
    if (extname(file.path) === ".vue") {
      // Prepared SFCs carry their template and script prefixes; a Vue file
      // this run never analyzed, or one whose script cannot be read, can
      // hold dynamic classes or stylesheet loads it cannot see.
      // v-html injects runtime markup whose classes, like server-template
      // data, can never appear in the scan corpus.
      if (
        file.templatePrefixes === undefined ||
        file.sourceImportsUnverifiable === true ||
        file.templateInjectsMarkup === true
      ) {
        reservations.unbounded = true;
        continue;
      }
      // SFC style blocks never join the stylesheet snapshot, so their
      // authored selectors reserve here; a block no supported syntax can
      // parse stays opaque.
      const styleRanges = context.vueStyleRanges.get(file.path);
      if (styleRanges === undefined) {
        reservations.unbounded = true;
        continue;
      }
      const fileBytes = Buffer.from(file.source);
      for (const [index, range] of styleRanges.entries()) {
        // An external style source resolving into the snapshot is scanned
        // there; anything else loads a sheet this run never sees.
        if (range.src !== undefined) {
          const resolved = range.src.startsWith(".")
            ? resolve(dirname(file.path), range.src)
            : undefined;
          if (resolved === undefined || !context.styleSources.has(resolved)) {
            reservations.unbounded = true;
          }
          continue;
        }
        const block = fileBytes.subarray(range.start, range.end).toString();
        // The declared language is authoritative: a permissive parser can
        // accept another syntax's dependency at-rules without recording
        // them, silently dropping reservation input.
        if (!["css", "scss", "sass", "less"].includes(range.lang)) {
          reservations.unbounded = true;
          continue;
        }
        // A block dependency outside the snapshot loads selectors the
        // scan cannot see, exactly like a stylesheet's own imports.
        scanStylesheetReservations(
          `${file.path}.block.${index}.${range.lang}`,
          block,
          context.styleSources,
          reservations,
        );
      }
      for (const prefix of file.templatePrefixes) reservations.prefixes.add(prefix);
      // Decoded class tokens the raw scan cannot read reserve complete
      // spellings, matching the HTML entity rule.
      for (const token of file.templateClassTokens ?? []) reservations.names.add(token);
      // Rendered template stylesheet links load sheets like HTML links.
      if (file.templateStylesheetLinksUnverifiable === true) reservations.unbounded = true;
      for (const link of file.templateStylesheetLinks ?? []) {
        if (opaqueStylesheetLink(link)) reservations.unbounded = true;
      }
      opaqueStylesheetImports(file.sourceImports ?? []);
      continue;
    }
    if (
      ![".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"].includes(extname(file.path))
    )
      continue;
    try {
      const analysis = sourceAnalysis(file.path, file.source);
      for (const prefix of analysis.templatePrefixes) {
        reservations.prefixes.add(prefix);
      }
      for (const property of analysis.customProperties) reservations.properties.add(property);
      if (analysis.customPropertiesUnbounded) reservations.propertiesUnbounded = true;
      opaqueStylesheetImports(analysis.imports);
      // A rendered <style> element carries selectors this analysis never
      // reads, matching the HTML hasStyle rule.
      if (analysis.rendersStyleElement) reservations.unbounded = true;
      // A rendered stylesheet link loads a sheet like an HTML document
      // link: a remote, unresolved, or other-entry target is opaque.
      if (analysis.stylesheetLinksUnverifiable) reservations.unbounded = true;
      for (const link of analysis.stylesheetLinks) {
        if (opaqueStylesheetLink(link)) reservations.unbounded = true;
      }
    } catch {
      // An unparseable source can dynamically supply any class or load
      // any stylesheet, matching the HTML and Vue failure paths; the
      // planner keeps such non-writable consumers as opaque references.
      reservations.unbounded = true;
    }
  }
  const recoverGroup = (error: unknown, fatal = false): PlanResult[] => {
    if (!options.force || fatal) throw error;
    // Group-level failures affect every package depending on the shared
    // entry, so the complete group is skipped together.
    for (const member of active) {
      results.push({ failure: packageFailure(workspaceRoot, member.packageRoot, error) });
    }
    return results;
  };

  // A member's stylesheet consumed by another package's files must prove
  // that those consumers load the shared entry too: through the consumer
  // package's own dual proofs when it shares the entry, trivially when the
  // consumer package owns this entry, and never from outside the group.
  const membersByRoot = new Map(active.map((member) => [member.packageRoot, member]));
  for (const member of options.extractMediaQueries !== false ? active : []) {
    member.stylesheets = member.stylesheets.filter((stylesheet) => {
      const foreignConsumers = member.files.filter(
        (file) =>
          context.pathOwners.get(file.path) !== member.packageRoot &&
          (importsStylesheet(file, stylesheet.cssPath) ||
            (file.htmlStylesheets ?? []).some((linked) => linked.cssPath === stylesheet.cssPath)),
      );
      const proven = foreignConsumers.every((consumer) => {
        const consumerRoot = context.pathOwners.get(consumer.path);
        const consumerMember =
          consumerRoot === undefined ? undefined : membersByRoot.get(consumerRoot);
        if (consumerMember === undefined) return false;
        if (consumerMember.sharedProofs === undefined) return true;
        return consumerMember.sharedProofs.provenConsumer(consumer);
      });
      if (!proven) {
        member.warnings.push({
          code: "unproven-shared-entry-flow",
          file: stylesheet.cssPath,
          start: 0,
          end: 0,
          message:
            "A consumer flow could not be proven to load the shared Tailwind entry, so the stylesheet is retained.",
        });
      }
      return proven;
    });
  }

  // A member whose stylesheets were all retained by the proofs still
  // surfaces its warnings, but plans nothing.
  for (const member of active.filter((remaining) => remaining.stylesheets.length === 0)) {
    if (member.warnings.length > 0) {
      results.push({
        plan: {
          files: [],
          deletedFiles: [],
          unlinkedFiles: [],
          candidates: [],
          rules: [],
          // Preparation warnings surface here because finalization never
          // runs for a fully retained member.
          warnings: [
            ...member.warnings,
            ...member.preparedHtml.warnings,
            ...member.preparedVue.warnings,
          ],
          convertedRules: 0,
          retainedRules: 0,
        },
      });
    }
  }
  active = active.filter((remaining) => remaining.stylesheets.length > 0);
  if (active.length === 0) return results;

  // Entry safety gates every entry mutation, not only media extraction.
  const groupWritable = !(await entryUnsafe(entry.path, workspaceRoot, targetable));
  const mediaWarnings: MigrationWarning[] = [];
  let extraction: MediaExtraction | undefined;
  // Malformed member inputs must stay attributable to their member even
  // when the group batch would be the first to parse them: the collection
  // pass doubles as per-member input isolation, so it always runs and
  // under force only the failing member is skipped.
  const collections: MediaCollection[] = [];
  // Reassigning `active` below does not disturb this iteration over the
  // current array, and each member is visited exactly once.
  for (const member of active) {
    try {
      collections.push(collectMemberMediaConditions(member.stylesheets, entry));
    } catch (error) {
      if (!options.force || !isRecoverablePlanningError(error)) throw error;
      results.push({ failure: packageFailure(workspaceRoot, member.packageRoot, error) });
      active = active.filter((remaining) => remaining !== member);
    }
  }
  if (active.length === 0) return results;
  let names = extraction?.names;
  let generated = extraction?.generated ?? [];
  if (options.extractMediaQueries !== false) {
    if (!groupWritable) {
      // The promised fallback is arbitrary variants, not the legacy
      // conversions: an authoritative empty map sends every condition to
      // its arbitrary form, because nothing about the unsafe entry's
      // variants was verified. The warning only applies when there is
      // media behavior to preserve.
      names = {};
      generated = [];
      if (collections.some((collection) => collection.components.length > 0)) {
        mediaWarnings.push({
          code: "media-query-definition-fallback",
          file: entry.path,
          start: 0,
          end: 0,
          message:
            "The Tailwind entry could not be edited safely, so media behavior was preserved with arbitrary variants.",
        });
      }
    } else {
      try {
        extraction = planMediaExtraction({
          collections,
          tailwind: entry,
          // The entry's effective scan corpus covers the whole discovered
          // workspace, not only active members' planner files: an inert
          // candidate on an untouched sibling page must still reserve its
          // variant names. Over-reservation is harmless.
          scannedSources: context.sourceFiles.map((file) => file.source),
        });
      } catch (error) {
        return recoverGroup(error, !isRecoverablePlanningError(error));
      }
      names = extraction.names;
      generated = extraction.generated;
    }
  }

  let plan: Plan = {
    files: [],
    deletedFiles: [],
    unlinkedFiles: [],
    candidates: [],
    rules: [],
    warnings: [],
    convertedRules: 0,
    retainedRules: 0,
  };
  let augmented = entry.css;
  const blocked: BlockedRules = new Map();
  let candidateAliases: Record<string, string> | undefined;
  let canonicalized = false;
  let fontTokens: Record<string, string> = {};
  let fontFailures = new Set<string>();
  let composeEntry: (tokens: Record<string, string>) => string = () => entry.css;

  // The whole group plans as one native batch, so cross-member conflicts
  // on shared consumers go through the same analysis as conflicts inside
  // one package. The finalization loop below can drop a member and replan.
  finalization: for (;;) {
    // Order-sensitive at-rule registrations whose moves would need an
    // unprovable precedence stay in place: everything already in the entry
    // graph, every stylesheet that cannot move its at-rules, and every
    // movable stylesheet colliding with another one. Within one stylesheet
    // moved blocks keep their source order, so only cross-stylesheet
    // collisions gate.
    const baseIdentities = new Set(
      atRuleIdentities(
        `${entry.path}.group-base.css`,
        [entry.css, ...entry.graphSources.map((graphSource) => graphSource.source)].join("\n"),
      ),
    );
    const sheets = active.flatMap((member) =>
      member.stylesheets.map((stylesheet) => ({ member, stylesheet })),
    );
    const movable = ({ member, stylesheet }: (typeof sheets)[number]): boolean =>
      member.sharedProofs === undefined &&
      stylesheet.isModule === true &&
      (stylesheet.syntax === undefined || stylesheet.syntax === "css") &&
      (stylesheet.vueBlocks === undefined || stylesheet.vueBlocks.length === 0);
    for (const sheet of sheets) {
      if (movable(sheet)) continue;
      for (const identity of stylesheetAtRuleIdentities(sheet.stylesheet)) {
        baseIdentities.add(identity);
      }
    }
    const sheetIdentities = sheets.map((sheet) =>
      movable(sheet) ? new Set(stylesheetAtRuleIdentities(sheet.stylesheet)) : new Set<string>(),
    );
    const opaqueVueRegistrations = sheets.some(
      ({ stylesheet }) => stylesheet.vueAtRuleIdentities === null,
    );
    const movesDisabled = new Set<number>(
      opaqueVueRegistrations
        ? sheets.flatMap((sheet, index) => (movable(sheet) ? [index] : []))
        : [],
    );
    for (const [index, identities] of sheetIdentities.entries()) {
      for (const identity of identities) {
        if (baseIdentities.has(identity)) movesDisabled.add(index);
        for (const [other, otherIdentities] of sheetIdentities.entries()) {
          if (other !== index && otherIdentities.has(identity)) {
            movesDisabled.add(index);
            movesDisabled.add(other);
          }
        }
      }
    }

    // Every member contributes its prepared view of the shared file
    // corpus. The version prepared by a file's owning package carries the
    // right element metadata, while stylesheet-link contexts merge across
    // members: a page owned by one package may consume another member's
    // stylesheet through a link only that member's preparation resolved.
    const filesByPath = new Map<string, PlannedFile>();
    const contextsByPath = new Map<string, HtmlContext[]>();
    for (const member of active) {
      for (const file of member.files) {
        if (file.htmlStylesheets !== undefined && file.htmlStylesheets.length > 0) {
          const contexts = contextsByPath.get(file.path) ?? [];
          contexts.push(...file.htmlStylesheets);
          contextsByPath.set(file.path, contexts);
        }
        if (
          !filesByPath.has(file.path) ||
          context.pathOwners.get(file.path) === member.packageRoot
        ) {
          filesByPath.set(file.path, file);
        }
      }
    }
    const files = [...filesByPath.values()]
      .map((file) => {
        const contexts = contextsByPath.get(file.path);
        if (contexts === undefined) return file;
        const seen = new Set<string>();
        const merged = contexts.filter((linked) => {
          const key = `${linked.cssPath}\0${linked.variants.join(",")}\0${linked.direct}\0${linked.analyzable}`;
          if (seen.has(key)) return false;
          seen.add(key);
          return true;
        });
        return { ...file, htmlStylesheets: merged };
      })
      .sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0));

    planning: for (;;) {
      const request: PlannerRequest = {
        stylesheets: sheets.map(({ member, stylesheet }, index) => {
          const rules = blocked.get(index);
          const movesOff = member.sharedProofs !== undefined || movesDisabled.has(index);
          return {
            ...stylesheet,
            ...(rules
              ? { blockedRules: [...rules.values()].map((blockedRule) => blockedRule.ruleId) }
              : {}),
            // Actively applied global at-rules from an ancestor-shared
            // member would activate in every other flow loading the
            // entry, and colliding registrations have no proven order.
            ...(movesOff ? { globalAtRuleMoves: false } : {}),
          };
        }),
        tailwindPath: entry.path,
        tailwindSource: entry.css,
        utilityPrefix: entry.designSystem.theme.prefix,
        themeTokens: entry.themeTokens,
        ...(names ? { mediaNames: names } : {}),
        ...(candidateAliases ? { candidateAliases } : {}),
        ...(groupWritable ? {} : { entryWritable: false }),
        files,
      };
      try {
        plan = JSON.parse(planBatchMigration(JSON.stringify(request)));
      } catch (error) {
        // The collection pre-pass already isolated per-member input
        // failures, so a batch failure affects the shared plan itself.
        return recoverGroup(error, !isRecoverablePlanningError(error));
      }
      // The batch chains entry additions internally and returns one final
      // entry source; the group re-emits it as its own single claim.
      let composed = entry.css;
      const plannedEntry = plan.files.findIndex((file) => file.path === entry.path);
      if (plannedEntry >= 0) {
        if (!groupWritable) {
          throw new Error(`The planner claimed an unwritable Tailwind entry: ${entry.path}`);
        }
        composed = plan.files[plannedEntry].source;
        plan.files.splice(plannedEntry, 1);
        if (!composed.startsWith(entry.css)) {
          throw new Error(
            `The planner rewrote the Tailwind entry instead of appending: ${entry.path}`,
          );
        }
      }

      const currentExtraction = names !== undefined ? { names, generated } : undefined;
      const used = currentExtraction ? usedGeneratedDefinitions(currentExtraction, plan) : [];
      // Entry additions follow the RFC order: planner-owned moved
      // definitions, the generated font theme block, generated media
      // custom variants. The composer is kept so post-loop token pruning
      // can rebuild the entry without another planning pass.
      composeEntry = (tokens) => {
        const withFonts = appendFontTheme(composed, tokens);
        return currentExtraction ? appendMediaDefinitions(withFonts, used) : withFonts;
      };
      augmented = composeEntry(fontTokens);
      let system: DesignSystem;
      try {
        system = augmented === entry.css ? entry.designSystem : await entry.loadWith(augmented);
      } catch (error) {
        if (isIntegrityError(error)) throw error;
        // Isolate generated definitions before declaring the composed base
        // invalid: fall every generated condition back to its arbitrary
        // form and replan. A failure with no generated definition left
        // comes from the composed base or another planner defect.
        if (names !== undefined && generated.length > 0) {
          const generatedKeys = new Set(generated.map((definition) => definition.key));
          names = Object.fromEntries(
            Object.entries(names).filter(([key]) => !generatedKeys.has(key)),
          );
          generated = [];
          continue planning;
        }
        return recoverGroup(error);
      }
      if (currentExtraction && names !== undefined) {
        const prefix = entry.designSystem.theme.prefix;
        const rejected = new Set(
          used
            .filter(
              ({ name }) => system.candidatesToCss([probeCandidate(prefix, name)])[0] === null,
            )
            .map(({ key }) => key),
        );
        if (rejected.size > 0) {
          names = Object.fromEntries(Object.entries(names).filter(([key]) => !rejected.has(key)));
          generated = generated.filter(({ key }) => !rejected.has(key));
          continue planning;
        }
      }

      // A candidate Tailwind refuses to compile retains its owning rule(s)
      // instead of aborting the run: block those rules and replan until
      // every applied candidate compiles.
      const replanSystem = augmented === entry.css ? entry.designSystem : system;
      const failing = invalidCandidates(replanSystem, plan.candidates);
      if (failing.length > 0) {
        if (!accumulateBlockedRules(blocked, plan, failing)) {
          throw new Error(`Tailwind did not generate CSS for candidate: ${failing[0]}`);
        }
        continue planning;
      }
      // One bounded canonicalization replan: the provisional system above
      // already contains every generated media definition, so complete
      // probes canonicalize with their variants, and accepted aliases are
      // rendered by the planner on the next pass.
      if (!canonicalized) {
        canonicalized = true;
        const aliases = acceptedCandidateAliases(
          replanSystem,
          plan.candidateProbes ?? [],
          reservations,
        );
        // Font registration shares the single bounded replan; an
        // unwritable entry cannot hold new tokens, and its retention
        // warnings are planner-side outcomes.
        if (groupWritable) {
          const fonts = await registerFontTokens({
            system: replanSystem,
            entryCss: augmented,
            loadWith: entry.loadWith,
            themeTokens: entry.themeTokens,
            probes: plan.fontFamilyProbes ?? [],
            corpus: [...new Set([...plan.candidates, ...(plan.candidateProbes ?? [])])],
            reservations,
            existingAliases: aliases,
          });
          Object.assign(aliases, fonts.aliases);
          fontTokens = fonts.tokens;
          fontFailures = fonts.failures;
        }
        if (Object.keys(aliases).length > 0) {
          candidateAliases = aliases;
          continue planning;
        }
      }
      break;
    }

    // Retained rules whose font stacks needed a token explain why: an
    // unwritable entry cannot hold one, and a failed registration could
    // not prove one safe. A rule that converted with its arbitrary
    // spelling needs no explanation.
    const ruleStatus = new Map(
      plan.rules.map((rule) => [
        `${rule.stylesheet}:${rule.ruleId.start}-${rule.ruleId.end}`,
        rule.status,
      ]),
    );
    const warned = new Set<string>();
    for (const probe of plan.fontFamilyProbes ?? []) {
      if (probe.firstFamily.kind !== "name") continue;
      const ruleKey = `${probe.stylesheet}:${probe.ruleId.start}-${probe.ruleId.end}`;
      if (ruleStatus.get(ruleKey) !== "retained" || warned.has(ruleKey)) continue;
      const failed = fontFailures.has(probe.candidate);
      if (groupWritable && !failed) continue;
      warned.add(ruleKey);
      plan.warnings.push({
        code: groupWritable ? "font-theme-registration-failed" : "font-theme-registration-required",
        file: sheets[probe.stylesheet].stylesheet.cssPath,
        start: probe.authoredSpan.start,
        end: probe.authoredSpan.end,
        message: groupWritable
          ? "A safe Tailwind font theme token could not be registered, so the rule is retained."
          : "The font family requires a Tailwind theme token, but the Tailwind entry cannot be edited, so the rule is retained.",
      });
    }

    // A generated token stays in the entry only while an applied
    // candidate references its utility; removing an unused token cannot
    // affect an applied candidate, so no further planning pass runs.
    if (Object.keys(fontTokens).length > 0) {
      const applied = new Set(
        plan.candidates.map((candidate) => utilitySegment(candidate).replace(/!$/, "")),
      );
      const pruned = Object.fromEntries(
        Object.entries(fontTokens).filter(([name]) => applied.has(`font-${name}`)),
      );
      if (Object.keys(pruned).length !== Object.keys(fontTokens).length) {
        fontTokens = pruned;
        augmented = composeEntry(pruned);
      }
    }

    for (const [index, rules] of blocked) {
      const cssPath = sheets[index].stylesheet.cssPath;
      for (const { authoredSpan, candidates } of rules.values()) {
        const failed = [...candidates]
          .sort()
          .map((candidate) => `\`${candidate}\``)
          .join(", ");
        plan.warnings.push({
          code: "candidate-compilation-failure",
          file: cssPath,
          // ruleId is a compiled-domain span; warnings anchor to the
          // authored file.
          start: authoredSpan.start,
          end: authoredSpan.end,
          message: `Tailwind did not generate CSS for ${failed}, so the rule is retained.`,
        });
      }
    }
    plan.warnings.push(...mediaWarnings);
    for (const member of active) plan.warnings.push(...member.warnings);
    removeGroupHtmlLinks(plan, active);

    // Finalization can still fail recoverably under force (for example a
    // missing preprocessor discovered during post-edit verification). The
    // failed file's owning member is dropped and the group replans from
    // the snapshot, bounded by the member count.
    const failure = await finishGroupPlan(context, active, plan);
    if (failure === undefined) break;
    if (!options.force || failure.fatal) throw failure.error;
    const failed = failure.member ?? active[0];
    results.push({ failure: packageFailure(workspaceRoot, failed.packageRoot, failure.error) });
    active = active.filter((member) => member !== failed);
    if (active.length === 0) return results;
    blocked.clear();
    plan.warnings = [];
    continue finalization;
  }

  // Duplicated preparation warnings from members sharing a foreign page
  // collapse inside the single group plan.
  const seenWarnings = new Set<string>();
  plan.warnings = plan.warnings.filter((warning) => {
    const key = `${warning.code}\0${warning.file}\0${warning.start}\0${warning.end}\0${warning.message}`;
    if (seenWarnings.has(key)) return false;
    seenWarnings.add(key);
    return true;
  });
  results.push({ plan });

  if (augmented !== entry.css) {
    results.push({
      plan: {
        files: [{ path: entry.path, source: augmented }],
        deletedFiles: [],
        unlinkedFiles: [],
        candidates: [],
        rules: [],
        warnings: [],
        convertedRules: 0,
        retainedRules: 0,
      },
    });
  }
  return results;
}

function accumulateBlockedRules(blocked: BlockedRules, plan: Plan, failing: string[]): boolean {
  let progressed = false;
  for (const rule of plan.rules) {
    const failed = rule.candidates.filter((candidate) => failing.includes(candidate));
    if (failed.length === 0) continue;
    // Keyed by entry index, not cssPath: same-path Vue entries (scoped and
    // module blocks) reuse local rule spans, so path-level attribution
    // would block unrelated rules in the sibling entry.
    let rules = blocked.get(rule.stylesheet);
    if (!rules) blocked.set(rule.stylesheet, (rules = new Map()));
    const key = `${rule.ruleId.start}-${rule.ruleId.end}`;
    let blockedRule = rules.get(key);
    if (!blockedRule) {
      rules.set(
        key,
        (blockedRule = {
          ruleId: rule.ruleId,
          authoredSpan: rule.authoredSpan,
          candidates: new Set(),
        }),
      );
      progressed = true;
    }
    for (const candidate of failed) blockedRule.candidates.add(candidate);
  }
  return progressed;
}

/// True when the group must not edit the entry: a symbolic link or a path
/// outside the writable project scope.
async function entryUnsafe(
  entryPath: string,
  workspaceRoot: string,
  targetable: Set<string>,
): Promise<boolean> {
  if (!targetable.has(entryPath) || !isProjectInput(workspaceRoot, entryPath)) return true;
  const stats = await lstat(entryPath);
  return stats.isSymbolicLink();
}

/// Registration identities of order-sensitive movable global at-rules in a
/// CSS fragment, in source order and with duplicates preserved. Moved
/// keyframes are renamed to unique migration names and never collide;
/// `@font-face` has no prelude and is identified by its font-family
/// descriptor.
function atRuleIdentities(key: string, css: string): string[] {
  return css.trim() === "" ? [] : stylesheetAnalysis(key, css).globalAtRuleIdentities;
}

/// Analysis keys carry each sheet's real identity so the native cache can
/// serve repeated scans, including across finalization replans; a shared
/// synthetic key would thrash the one cache slot on every distinct sheet.
function stylesheetAtRuleIdentities(stylesheet: StylesheetEntry): string[] {
  if (stylesheet.vueAtRuleIdentities) return stylesheet.vueAtRuleIdentities;
  if (stylesheet.vueBlocks) {
    return stylesheet.vueBlocks.flatMap((block, index) =>
      atRuleIdentities(
        `${stylesheet.cssPath}.${index}.at-rules.${block.analysisSource ? "css" : block.syntax}`,
        block.analysisSource ?? block.content,
      ),
    );
  }
  return stylesheet.analysisSource === undefined
    ? atRuleIdentities(
        `${stylesheet.cssPath}.at-rules.${stylesheet.syntax ?? "css"}`,
        stylesheet.cssSource,
      )
    : atRuleIdentities(`${stylesheet.cssPath}.at-rules.css`, stylesheet.analysisSource);
}

// The per-member planning tail: HTML link removal, planned-source
// verification, and preprocessor recompilation checks.
interface GroupFinishFailure {
  error: unknown;
  /// The member whose file or stylesheet failed, when attributable.
  member?: PreparedPackage;
  fatal: boolean;
}

// The group planning tail: preparation warnings, planned-source
// verification, and preprocessor recompilation checks over the single
// group plan. Failures attribute to the owning member so force can drop
// it and replan the rest.
async function finishGroupPlan(
  context: MigrationContext,
  members: PreparedPackage[],
  plan: Plan,
): Promise<GroupFinishFailure | undefined> {
  const { snapshots } = context;
  const membersByRoot = new Map(members.map((member) => [member.packageRoot, member]));
  const ownerOf = (path: string): PreparedPackage | undefined => {
    const root = context.pathOwners.get(path);
    return root === undefined ? undefined : membersByRoot.get(root);
  };
  for (const member of members) {
    plan.warnings.push(...member.preparedHtml.warnings);
    plan.warnings.push(...member.preparedVue.warnings);
  }
  try {
    for (const file of plan.files.filter((file) => extname(file.path) === ".html")) {
      try {
        parseHtmlSource(file.path, file.source);
      } catch (error) {
        // The original page parsed during preparation, so a planned source
        // that does not is an invalid plan: fatal even under force.
        return { error, member: ownerOf(file.path), fatal: true };
      }
    }
    for (const file of plan.files.filter((file) => extname(file.path) === ".vue")) {
      const owner = ownerOf(file.path);
      const preparedVue = owner?.preparedVue ?? members[0].preparedVue;
      const vueCompiler = preparedVue.compiler ?? members[0].preparedVue.compiler;
      if (!vueCompiler) throw new Error(`No Vue compiler is available to verify ${file.path}`);
      try {
        const includeUnscoped = preparedVue.unscopedPaths.has(file.path);
        const blocks = verifyVueSource(vueCompiler, file.path, file.source, includeUnscoped);
        // Blocks the migration never edited are byte-identical to the
        // authored input and need no recompilation; a caller that only
        // received a template edit must not suddenly require a
        // preprocessor.
        const untouched = new Map<string, number>();
        for (const block of verifyVueSource(
          vueCompiler,
          file.path,
          snapshots.get(file.path) ?? "",
          includeUnscoped,
        )) {
          const key = `${block.syntax}\0${block.content}`;
          untouched.set(key, (untouched.get(key) ?? 0) + 1);
        }
        for (const [index, block] of blocks.entries()) {
          const key = `${block.syntax}\0${block.content}`;
          const remaining = untouched.get(key) ?? 0;
          if (remaining > 0) {
            untouched.set(key, remaining - 1);
            continue;
          }
          if (block.syntax === "css") {
            validateCss(block.content);
          } else {
            const compilerOwner = owner ?? members[0];
            validateCss(
              (
                await compileStyleEntry(
                  compilerOwner.styleCompilers,
                  compilerOwner.packageRoot,
                  `${file.path}.${index}.${block.syntax}`,
                  block.content,
                  { virtualEntry: true },
                )
              ).css,
            );
          }
        }
      } catch (error) {
        return { error, member: owner, fatal: !isMissingStyleCompilerError(error) };
      }
    }
    for (const member of members) {
      for (const stylesheet of member.stylesheets.filter((stylesheet) =>
        isPreprocessorPath(stylesheet.cssPath),
      )) {
        const changed = plan.files.find((file) => file.path === stylesheet.cssPath);
        if (!changed && !plan.deletedFiles.includes(stylesheet.cssPath)) continue;
        const source = changed?.source ?? "";
        try {
          validateCss(
            (
              await compileStyleEntry(
                member.styleCompilers,
                member.packageRoot,
                stylesheet.cssPath,
                source,
              )
            ).css,
          );
        } catch (error) {
          return { error, member, fatal: !isMissingStyleCompilerError(error) };
        }
      }
    }
  } catch (error) {
    return { error, fatal: !isMissingStyleCompilerError(error) };
  }
  return undefined;
}

// Every member's unlinked stylesheets strip their links from the page's
// final composed source inside the single group plan.
function removeGroupHtmlLinks(plan: Plan, members: PreparedPackage[]): void {
  const unlinked = new Set(plan.unlinkedFiles);
  const linksByFile = new Map<string, Set<string>>();
  const originals = new Map<string, string>();
  for (const member of members) {
    for (const link of member.preparedHtml.removableLinks) {
      if (!unlinked.has(link.cssPath)) continue;
      const links = linksByFile.get(link.filePath) ?? new Set();
      links.add(`${link.href}\0${link.media}`);
      linksByFile.set(link.filePath, links);
    }
    for (const file of member.preparedHtml.files) {
      if (!originals.has(file.path)) originals.set(file.path, file.source);
    }
  }

  for (const [filePath, removable] of linksByFile) {
    const planned = plan.files.find((file) => file.path === filePath);
    const source = planned?.source ?? originals.get(filePath);
    if (source === undefined) continue;
    const links = parseHtmlSource(filePath, source)
      .links.filter((link) => removable.has(`${link.href}\0${link.media}`))
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

function mergePlans(plans: Plan[], originals: Map<string, string>) {
  const filesByPath = new Map<string, SourceFile>();
  const deletedPaths = new Set<string>();
  const candidates = new Set<string>();
  const rules: PlanRule[] = [];
  const warnings: MigrationWarning[] = [];
  const seenWarnings = new Set<string>();
  let convertedRules = 0;
  let retainedRules = 0;
  const claimPath = (path: string, kind: string): void => {
    if (!originals.has(path))
      throw new Error(`Planned ${kind} is outside the source snapshot: ${path}`);
    if (filesByPath.has(path) || deletedPaths.has(path)) {
      throw new Error(`Multiple package groups planned changes for ${path}`);
    }
  };

  for (const plan of plans) {
    for (const file of plan.files) {
      claimPath(file.path, "file");
      filesByPath.set(file.path, file);
    }
    for (const path of plan.deletedFiles) {
      claimPath(path, "deletion");
      deletedPaths.add(path);
    }
    for (const candidate of plan.candidates) candidates.add(candidate);
    convertedRules += plan.convertedRules;
    retainedRules += plan.retainedRules;
    rules.push(...plan.rules);
    // Per-stylesheet planning repeats the same source-site warning once per
    // stylesheet; the user-facing report keeps the first of each.
    for (const warning of plan.warnings) {
      const key = `${warning.code}\0${warning.file}\0${warning.start}\0${warning.end}\0${warning.message}`;
      if (seenWarnings.has(key)) continue;
      seenWarnings.add(key);
      warnings.push(warning);
    }
  }
  return { filesByPath, deletedPaths, candidates, rules, warnings, convertedRules, retainedRules };
}
