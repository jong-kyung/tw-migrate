import { readFile } from "node:fs/promises";
import { basename, extname, join } from "node:path";

import { unifiedDiff } from "./util/diff.ts";
import { collectFiles, resolveScope } from "./discovery.ts";
import { parseHtmlSource } from "./parser/html.ts";
import { preparePackageHtml } from "./plan/html.ts";
import { planBatchMigration, validateCss } from "./native.ts";
import {
  indexStylesheetDependents,
  isIntegrityError,
  isProjectInput,
  isMissingStyleCompilerError,
  isRecoverablePlanningError,
  isStylesheetModule,
  isStylesheetPath,
  normalizedRelativePath,
  packageFailure,
  recordSnapshot,
  rejectSymlinkTarget,
  snapshotFile,
  snapshotLoadedSource,
  sourceReferencesStyle,
  stylesheetSyntax,
  SOURCE_EXTENSIONS,
} from "./util/shared.ts";
import { compileStyleEntry, isPreprocessorPath, isSassPath } from "./parser/style-compiler.ts";
import type { StyleCompilers } from "./parser/style-compiler.ts";
import { invalidCandidates, loadTailwind, resolveTailwindEntry } from "./tailwind.ts";
import { verifyVueSource } from "./parser/vue.ts";
import { preparePackageVue, vueWarningsOnlyResult } from "./plan/vue.ts";
import { verifySnapshots, writeChanges } from "./util/write.ts";
import type {
  LoadedTailwind,
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

interface WarningPosition {
  line: number;
  column: number;
}

function indexWarningPositions(
  source: string,
  offsets: Set<number>,
  unicodeSeparatorsAreNewlines: boolean,
  formFeedIsNewline: boolean,
  formFeedRanges: RuleSpan[] = [],
): Map<number, WarningPosition> {
  const targets = [...offsets].sort((left, right) => left - right);
  const positions = new Map<number, WarningPosition>();
  let target = 0;
  let byte = 0;
  let line = 1;
  let column = 1;
  let previousWasCarriageReturn = false;
  let formFeedRange = 0;
  const record = (): void => {
    while (targets[target] === byte) {
      positions.set(byte, { line, column });
      target += 1;
    }
  };

  record();
  for (const character of source) {
    const characterStart = byte;
    byte += Buffer.byteLength(character);
    while ((targets[target] ?? Infinity) < byte) target += 1;
    while ((formFeedRanges[formFeedRange]?.end ?? Infinity) <= characterStart) {
      formFeedRange += 1;
    }
    const range = formFeedRanges[formFeedRange];
    const formFeedInRange =
      range !== undefined && range.start <= characterStart && characterStart < range.end;
    if (character === "\r") {
      line += 1;
      column = 1;
      previousWasCarriageReturn = true;
    } else if (character === "\n") {
      if (!previousWasCarriageReturn) line += 1;
      column = 1;
      previousWasCarriageReturn = false;
    } else if (
      (unicodeSeparatorsAreNewlines && (character === "\u2028" || character === "\u2029")) ||
      ((formFeedIsNewline || formFeedInRange) && character === "\f")
    ) {
      line += 1;
      column = 1;
      previousWasCarriageReturn = false;
    } else {
      column += character.length;
      previousWasCarriageReturn = false;
    }
    record();
  }
  return positions;
}

function warningLocation(
  positions: Map<number, WarningPosition> | undefined,
  start: number,
  end: number,
): Pick<MigrationWarning, "line" | "column" | "endLine" | "endColumn"> {
  if ((start === 0 && end === 0) || !positions) return {};
  const from = positions.get(start);
  const to = positions.get(end);
  return from && to
    ? { line: from.line, column: from.column, endLine: to.line, endColumn: to.column }
    : {};
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
        const mayReferenceModule = extname(path) !== ".html" || /<link\b/i.test(source);
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

  const vueStyleRanges = new Map<string, RuleSpan[]>();
  const context: MigrationContext = {
    ...scope,
    options,
    snapshots,
    styleSources,
    sourceFiles,
    styleDependents: indexStylesheetDependents(styleSources),
    vueStyleRanges,
  };
  const failures: MigrationFailure[] = [];
  const plans: Plan[] = [];
  for (const packageRoot of selectedPackages) {
    const result = await planPackage(context, packageRoot);
    if (result.failure) failures.push(result.failure);
    else if (result.plan) plans.push(result.plan);
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
      return [
        [
          file,
          {
            default: indexWarningPositions(
              source,
              offsets,
              javascriptSource,
              isStylesheetPath(file),
            ),
            style:
              styleRanges.length > 0
                ? indexWarningPositions(source, offsets, false, false, styleRanges)
                : undefined,
            styleRanges,
          },
        ] as const,
      ];
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
      const styleWarning = indexed?.styleRanges.some(
        (range) => range.start <= warning.start && warning.start < range.end,
      );
      return {
        ...warning,
        ...warningLocation(
          styleWarning ? indexed?.style : indexed?.default,
          warning.start,
          warning.end,
        ),
        file: normalizedRelativePath(cwd, warning.file),
      };
    }),
    failures,
  };
}

async function planPackage(context: MigrationContext, packageRoot: string): Promise<PlanResult> {
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
  for (const stylesheet of preparedVue.stylesheets) {
    const ranges = vueStyleRanges.get(stylesheet.cssPath) ?? [];
    ranges.push(
      ...(stylesheet.vueBlocks ?? []).map((block) => ({
        start: block.contentStart,
        end: block.contentEnd,
      })),
    );
    ranges.sort((left, right) => left.start - right.start);
    vueStyleRanges.set(stylesheet.cssPath, ranges);
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

  let tailwindPath: string;
  let tailwindEntries: string[];
  try {
    ({ path: tailwindPath, entries: tailwindEntries } = resolveTailwindEntry(
      ownedStyles,
      styleSources,
      configuredEntry,
    ));
  } catch (error) {
    return recover(error);
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
              packageSources.some((file) => sourceReferencesStyle(file, path))),
        );
  if (targets.length === 0 && preparedVue.stylesheets.length === 0) {
    return vueWarningsOnlyResult(preparedVue);
  }
  if (targets.some((path) => excludedEntries.has(path))) {
    throw new Error("The Tailwind CSS entry cannot be migrated.");
  }

  let tailwind;
  try {
    tailwind = await loadTailwind(packageRoot, tailwindPath, snapshots, workspaceRoot);
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
    for (const stylePath of targets.sort()) {
      await rejectSymlinkTarget(stylePath, packageRoot);
      const isPartial = isSassPath(stylePath) && basename(stylePath).startsWith("_");
      const stylesheet: StylesheetEntry = {
        cssPath: stylePath,
        cssSource: styleSources.get(stylePath) ?? "",
        cssModuleId: normalizedRelativePath(packageRoot, stylePath),
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

  stylesheets.push(...preparedVue.stylesheets);
  const request: PlannerRequest = {
    stylesheets,
    tailwindPath: tailwind.path,
    tailwindSource: tailwind.css,
    utilityPrefix: tailwind.designSystem.theme.prefix,
    themeTokens: tailwind.themeTokens,
    files,
  };
  let plan: Plan;
  try {
    plan = JSON.parse(planBatchMigration(JSON.stringify(request)));
  } catch (error) {
    return recover(error, !isRecoverablePlanningError(error));
  }

  try {
    plan = replanCompileFailures(tailwind, request, plan);
  } catch (error) {
    return recover(error);
  }

  removeMigratedHtmlLinks(plan, preparedHtml);
  plan.warnings.push(...preparedHtml.warnings);
  plan.warnings.push(...preparedVue.warnings);
  try {
    for (const file of plan.files.filter((file) => extname(file.path) === ".html")) {
      parseHtmlSource(file.path, file.source);
    }
    for (const file of plan.files.filter((file) => extname(file.path) === ".vue")) {
      const vueCompiler = preparedVue.compiler;
      if (!vueCompiler) throw new Error(`No Vue compiler is available to verify ${file.path}`);
      const includeUnscoped = preparedVue.unscopedPaths.has(file.path);
      const blocks = verifyVueSource(vueCompiler, file.path, file.source, includeUnscoped);
      // Blocks the migration never edited are byte-identical to the authored
      // input and need no recompilation; a caller that only received a
      // template edit must not suddenly require a preprocessor.
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
          validateCss(
            (
              await compileStyleEntry(
                styleCompilers,
                packageRoot,
                `${file.path}.${index}.${block.syntax}`,
                block.content,
                { virtualEntry: true },
              )
            ).css,
          );
        }
      }
    }
    for (const stylesheet of stylesheets.filter((stylesheet) =>
      isPreprocessorPath(stylesheet.cssPath),
    )) {
      const changed = plan.files.find((file) => file.path === stylesheet.cssPath);
      if (!changed && !plan.deletedFiles.includes(stylesheet.cssPath)) continue;
      const source = changed?.source ?? "";
      validateCss(
        (await compileStyleEntry(styleCompilers, packageRoot, stylesheet.cssPath, source)).css,
      );
    }
  } catch (error) {
    return recover(error, !isMissingStyleCompilerError(error));
  }
  return { plan };
}

// A candidate Tailwind refuses to compile retains its owning rule(s) instead
// of aborting the run: block those rules and replan until every applied
// candidate compiles. Each iteration blocks at least one new rule, so the
// loop is bounded by the rule count; if a failing candidate cannot be
// attributed to a new rule, fall back to the package-level failure path.
function replanCompileFailures(
  tailwind: LoadedTailwind,
  request: PlannerRequest,
  initialPlan: Plan,
): Plan {
  let plan = initialPlan;
  const blockedByStylesheet = new Map<
    number,
    Map<string, { ruleId: RuleSpan; authoredSpan: RuleSpan; candidates: Set<string> }>
  >();
  while (true) {
    const failing = invalidCandidates(tailwind, plan.candidates);
    if (failing.length === 0) break;
    let progressed = false;
    for (const rule of plan.rules) {
      const failed = rule.candidates.filter((candidate) => failing.includes(candidate));
      if (failed.length === 0) continue;
      // Keyed by entry index, not cssPath: same-path Vue entries (scoped and
      // module blocks) reuse local rule spans, so path-level attribution
      // would block unrelated rules in the sibling entry.
      let blocked = blockedByStylesheet.get(rule.stylesheet);
      if (!blocked) blockedByStylesheet.set(rule.stylesheet, (blocked = new Map()));
      const key = `${rule.ruleId.start}-${rule.ruleId.end}`;
      let entry = blocked.get(key);
      if (!entry) {
        blocked.set(
          key,
          (entry = { ruleId: rule.ruleId, authoredSpan: rule.authoredSpan, candidates: new Set() }),
        );
        progressed = true;
      }
      for (const candidate of failed) entry.candidates.add(candidate);
    }
    if (!progressed) {
      throw new Error(`Tailwind did not generate CSS for candidate: ${failing[0]}`);
    }
    plan = JSON.parse(
      planBatchMigration(
        JSON.stringify({
          ...request,
          stylesheets: request.stylesheets.map((stylesheet, index) => ({
            ...stylesheet,
            blockedRules: [...(blockedByStylesheet.get(index)?.values() ?? [])].map(
              (entry) => entry.ruleId,
            ),
          })),
        }),
      ),
    );
  }
  for (const [index, blocked] of blockedByStylesheet) {
    const cssPath = request.stylesheets[index].cssPath;
    for (const { authoredSpan, candidates } of blocked.values()) {
      const failed = [...candidates]
        .sort()
        .map((candidate) => `\`${candidate}\``)
        .join(", ");
      plan.warnings.push({
        code: "candidate-compilation-failure",
        file: cssPath,
        // ruleId is a compiled-domain span; warnings anchor to the authored file.
        start: authoredSpan.start,
        end: authoredSpan.end,
        message: `Tailwind did not generate CSS for ${failed}, so the rule is retained.`,
      });
    }
  }
  return plan;
}

function removeMigratedHtmlLinks(plan: Plan, preparedHtml: PreparedHtml): void {
  const unlinked = new Set(plan.unlinkedFiles);
  const linksByFile = new Map<string, Set<string>>();
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
