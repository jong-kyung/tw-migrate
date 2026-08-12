import { dirname, extname, join, resolve } from "node:path";

import { sourceAnalysis, stylesheetAnalysis, validateCss } from "../native.ts";
import { parseHtmlSource } from "../parser/html.ts";
import { compileStyleEntry } from "../parser/style-compiler.ts";
import type { StyleCompilers } from "../parser/style-compiler.ts";
import { analyzeVueSource, loadProjectVueCompiler } from "../parser/vue.ts";
import {
  STYLESHEET_SYNTAX,
  cssImports,
  isProjectInput,
  isStylesheetModule,
  normalizedRelativePath,
  rejectSymlinkTarget,
  snapshotFile,
  snapshotLoadedSource,
  stylesheetReferenceTargets,
} from "../util/shared.ts";
import type {
  VueAnalysis,
  VueComponentEdge,
  VueStyleBlock,
  VueTemplateSite,
} from "../parser/vue.ts";
import type {
  MigrationWarning,
  PlanResult,
  PreparedSourceFile,
  PreparedVue,
  ShadowCssEntry,
  SourceFile,
  StylesheetEntry,
  VuePlannedElement,
} from "../types.ts";

function isLocalVueReference(reference: string): boolean {
  return (
    reference.startsWith(".") ||
    reference.startsWith("#") ||
    reference.includes("/") ||
    reference.includes(".vue")
  );
}

// An unresolved local reference only threatens the caller proof when it
// could actually name a Vue file: `.vue` spellings, extensionless paths, and
// aliases. An explicit foreign extension (`./utils.ts`) cannot.
function couldResolveToVue(reference: string): boolean {
  const path = reference.split(/[?#]/, 1)[0];
  return path.endsWith(".vue") || extname(path) === "";
}

function vueReferenceTarget(
  importer: string,
  reference: string,
  vuePaths: Set<string>,
): string | undefined {
  if (!reference.startsWith(".")) return undefined;
  const target = resolve(dirname(importer), reference);
  return [target, `${target}.vue`, join(target, "index.vue")].find((path) => vuePaths.has(path));
}

function vueGlobTargets(importer: string, patterns: string[], vuePaths: Set<string>): Set<string> {
  return new Set(
    patterns.flatMap((pattern) => {
      if (/[?*[\]{}]/.test(pattern)) {
        return pattern.includes("vue") || isLocalVueReference(pattern) ? [...vuePaths] : [];
      }
      const target = vueReferenceTarget(importer, pattern, vuePaths);
      if (target) return [target];
      return pattern.includes(".vue") ? [...vuePaths] : [];
    }),
  );
}

function normalizedVueTag(value: string): string {
  return value.replaceAll("-", "").toLowerCase();
}

function buildVueComponentGraph(
  ownedVue: SourceFile[],
  sourceFiles: SourceFile[],
  analyses: Map<string, VueAnalysis>,
  {
    styleSources,
    pathOwners,
    packageRoot,
  }: {
    styleSources: Map<string, string>;
    pathOwners: Map<string, string | undefined>;
    packageRoot: string;
  },
) {
  const vuePaths = new Set(ownedVue.map((file) => file.path));
  const callers = new Map([...vuePaths].map((path): [string, VueComponentEdge[]] => [path, []]));
  const callerOpen = new Set<string>();
  // A stylesheet import that does not resolve to a package-owned file (a
  // dependency or aliased stylesheet) still loads globally and can shadow
  // scoped deletions, but its selectors cannot be analyzed.
  let unresolvedStyleImport = false;
  const stylesheetReference = /\.(?:css|scss|sass|less)(?:$|[?#])/;

  for (const file of ownedVue) {
    const analysis = analyses.get(file.path);
    if (!analysis || analysis.retained) {
      for (const target of vuePaths) callerOpen.add(target);
      continue;
    }
    const bindings = analysis.componentImports.flatMap((entry) => {
      const target = vueReferenceTarget(file.path, entry.source, vuePaths);
      return target ? [{ ...entry, target }] : [];
    });
    analysis.resolvedComponents = analysis.componentSites.flatMap((site) => {
      const matches = bindings.filter(
        (entry) => normalizedVueTag(entry.local) === normalizedVueTag(site.tag),
      );
      if (matches.length !== 1) return [];
      const edge = { parent: file.path, child: matches[0].target, site };
      callers.get(edge.child)?.push(edge);
      if (site.idAttribute || analysis.dynamic) {
        callerOpen.add(edge.child);
        const childAnalysis = analyses.get(edge.child);
        if (childAnalysis) childAnalysis.rootIdsOverridden = true;
      }
      // A component rendered as a single-root parent's root chains the
      // parent's own fallthrough surface down to this child; that chain is
      // not modeled, so the child cannot be closed.
      if (analysis.alwaysRenderedRoots < 2 && analysis.rootStarts.includes(site.nodeStart)) {
        callerOpen.add(edge.child);
      }
      return [edge];
    });
    analysis.componentsOpen = analysis.resolvedComponents.length !== analysis.componentSites.length;
    analysis.setupImports = new Set(bindings.map((entry) => entry.target));
    if (analysis.componentsOpen) {
      for (const target of vuePaths) callerOpen.add(target);
    }
  }

  for (const file of sourceFiles) {
    let references: string[];
    let vueReferences: string[];
    let blockReferences: string[] = [];
    let hasDynamicImport = false;
    let vueGlobPatterns: string[] = [];
    let vueGlobUnverifiable = false;
    if (extname(file.path) === ".vue") {
      const fileAnalysis = analyses.get(file.path);
      if (fileAnalysis && !fileAnalysis.retained) {
        references = fileAnalysis.scriptStyleImports;
        vueReferences = fileAnalysis.scriptVueReferences;
        blockReferences = fileAnalysis.styleBlockImports.map((entry) => entry.reference);
        hasDynamicImport = fileAnalysis.scriptHasDynamicImport;
        vueGlobPatterns = fileAnalysis.scriptVueGlobPatterns;
        vueGlobUnverifiable = fileAnalysis.scriptVueGlobUnverifiable;
      } else {
        references = [];
        vueReferences = [];
        // Foreign SFCs require their owning package's Vue compiler. Until
        // their scripts are analyzed there, they cannot close caller surfaces.
        hasDynamicImport = true;
      }
    } else {
      try {
        const analysis = sourceAnalysis(file.path, file.source);
        references = analysis.staticImports;
        vueReferences = analysis.imports
          .filter((record) => !record.typeOnly)
          .map((record) => record.specifier);
        hasDynamicImport = analysis.hasDynamicImport;
        vueGlobPatterns = analysis.vueGlobPatterns;
        vueGlobUnverifiable = analysis.vueGlobUnverifiable;
      } catch {
        references = [];
        vueReferences = [];
        hasDynamicImport = true;
      }
    }
    if (
      hasDynamicImport ||
      vueGlobUnverifiable ||
      vueReferences.some(
        (reference) =>
          !stylesheetReference.test(reference) &&
          isLocalVueReference(reference) &&
          couldResolveToVue(reference) &&
          !vueReferenceTarget(file.path, reference, vuePaths),
      )
    ) {
      for (const target of vuePaths) callerOpen.add(target);
    }
    const resolvesOwned = (reference: string): boolean =>
      stylesheetReferenceTargets(file.path, reference, styleSources).some(
        (path) => pathOwners.get(path) === packageRoot,
      );
    if (
      pathOwners.get(file.path) === packageRoot &&
      (references.some(
        (reference) =>
          stylesheetReference.test(reference) &&
          // Bare script specifiers load package CSS this analysis cannot see.
          (!reference.startsWith(".") || !resolvesOwned(reference)),
      ) ||
        blockReferences.some((reference) => !resolvesOwned(reference)))
    ) {
      unresolvedStyleImport = true;
    }
    const imported = new Set(
      vueReferences.flatMap((reference) => {
        const target = vueReferenceTarget(file.path, reference, vuePaths);
        return target ? [target] : [];
      }),
    );
    for (const target of imported) {
      if (extname(file.path) !== ".vue" || !analyses.get(file.path)?.setupImports?.has(target)) {
        callerOpen.add(target);
      }
    }
    for (const target of vueGlobTargets(file.path, vueGlobPatterns, vuePaths)) {
      if (!imported.has(target)) callerOpen.add(target);
    }
  }

  return { callers, callerOpen, unresolvedStyleImport };
}

// Vue analysis can produce retention warnings even when nothing remains to
// plan (all blocks unsupported, unsupported Vue version); surface them
// through an otherwise empty plan instead of dropping them.
export function vueWarningsOnlyResult(
  preparedVue: PreparedVue,
  extraWarnings: MigrationWarning[] = [],
): PlanResult {
  const warnings = [...preparedVue.warnings, ...extraWarnings];
  if (warnings.length === 0) return {};
  return {
    plan: {
      files: [],
      deletedFiles: [],
      unlinkedFiles: [],
      candidates: [],
      rules: [],
      warnings,
      convertedRules: 0,
      retainedRules: 0,
    },
  };
}

// Lower this package's own Vue SFCs into their planner dual identity: one
// stylesheet entry per SFC with migratable plain-CSS scoped blocks, plus a
// consumer entry carrying the template's literal class sites. Files whose
// blocks or templates cannot be analyzed keep their raw source and warn.
export async function preparePackageVue({
  packageRoot,
  styleCompilers,
  sourceFiles,
  styleSources,
  pathOwners,
  targetable,
  explicitStyle,
  snapshots,
  workspaceRoot,
  workspaces,
  scannedPaths,
}: {
  packageRoot: string;
  styleCompilers: StyleCompilers;
  sourceFiles: SourceFile[];
  styleSources: Map<string, string>;
  pathOwners: Map<string, string | undefined>;
  targetable: Set<string>;
  explicitStyle?: string;
  snapshots: Map<string, string>;
  workspaceRoot: string;
  workspaces?: boolean;
  scannedPaths: string[];
}): Promise<PreparedVue> {
  const none: PreparedVue = {
    files: new Map(),
    stylesheets: [],
    styleRanges: new Map(),
    stylePaths: new Set(),
    unscopedPaths: new Set(),
    warnings: [],
    compiler: undefined,
  };
  // An explicit non-vue stylesheet selection plans only that stylesheet.
  if (explicitStyle && extname(explicitStyle) !== ".vue") return none;
  // Ignore filtering scopes what gets migrated, never what gets scanned: a
  // gitignored SFC's retained style blocks still shadow scoped deletions.
  const ownedVue = sourceFiles.filter(
    (file) => extname(file.path) === ".vue" && pathOwners.get(file.path) === packageRoot,
  );
  const selected = ownedVue.filter(
    (file) => targetable.has(file.path) && (!explicitStyle || file.path === explicitStyle),
  );
  if (selected.length === 0) return none;

  const loaded = await loadProjectVueCompiler(packageRoot);
  const warnings: MigrationWarning[] = [];
  if (loaded.unsupportedVersion || !loaded.compiler) {
    for (const file of selected) {
      warnings.push({
        code: "unsupported-vue-version",
        file: file.path,
        start: 0,
        end: 0,
        message: `Vue ${loaded.unsupportedVersion} is not supported; only Vue 3 SFCs are analyzed.`,
      });
    }
    return { ...none, warnings };
  }
  const compiler = loaded.compiler;

  // Every owned SFC is analyzed even under explicit selection: retained
  // style blocks anywhere in the package feed the cascade-shadow gate below.
  const analyses = new Map(
    ownedVue.map((file): [string, VueAnalysis] => [
      file.path,
      analyzeVueSource(compiler, file.path, file.source),
    ]),
  );
  const analysisOf = (path: string): VueAnalysis => {
    const analysis = analyses.get(path);
    if (!analysis) throw new Error(`No Vue analysis was recorded for ${path}`);
    return analysis;
  };
  const selectedPaths = new Set(selected.map((file) => file.path));
  for (const file of ownedVue) {
    const analysis = analysisOf(file.path);
    if (analysis.retained) continue;
    for (const edge of analysis.styleBlockImports) {
      const local = stylesheetReferenceTargets(file.path, edge.reference, styleSources).some(
        (path) => pathOwners.get(path) === packageRoot,
      );
      if (local) continue;
      analysis.escapeUnverifiable = true;
      if (selectedPaths.has(file.path)) {
        warnings.push({
          code: "unsupported-sfc-block",
          file: file.path,
          start: edge.start,
          end: edge.end,
          message: `The external style ${JSON.stringify(edge.reference)} could not be resolved inside the package.`,
        });
      }
    }
  }
  const compileBlocks = async (file: SourceFile, blocks: VueStyleBlock[]): Promise<void> => {
    for (const block of blocks.filter((block) => block.syntax !== "css")) {
      const virtualPath = `${file.path}.${block.syntax}`;
      const compiled = await compileStyleEntry(
        styleCompilers,
        packageRoot,
        virtualPath,
        block.content,
        { virtualEntry: true },
      );
      validateCss(compiled.css);
      for (const loadedPath of compiled.loadedPaths) {
        if (loadedPath === virtualPath || !isProjectInput(workspaceRoot, loadedPath)) continue;
        const source = await snapshotLoadedSource(snapshots, loadedPath);
        if (!styleSources.has(loadedPath)) styleSources.set(loadedPath, source);
        if (!pathOwners.has(loadedPath)) pathOwners.set(loadedPath, packageRoot);
      }
      block.analysisSource = compiled.css;
      block.sourcePath = virtualPath;
      block.sourceMappings = compiled.sourceMappings;
    }
  };
  for (const file of selected) {
    const analysis = analysisOf(file.path);
    if (!analysis.retained) {
      await compileBlocks(file, [...analysis.blocks, ...analysis.moduleBlocks]);
    }
  }
  const vueGraph = buildVueComponentGraph(ownedVue, sourceFiles, analyses, {
    styleSources,
    pathOwners,
    packageRoot,
  });

  // Non-scoped CSS in the package is unlayered and can outrank the layered
  // utilities that replace a deleted scoped rule. The planner retains any
  // scoped rule whose classes this pool also targets, so collect every
  // package stylesheet, HTML source (inline style blocks are never parsed),
  // and retained Vue style block.
  // The shadow corpus is a list of parseable CSS pieces whose selector
  // surface the planner indexes; anything whose selectors cannot be proven
  // (interpolation or `&`-concatenation in preprocessor text, inline HTML
  // style blocks, unanalyzable SFCs, unextractable escapes) marks the whole
  // corpus unverifiable and retains every closed deletion.
  const vueShadowCss: ShadowCssEntry[] = [];
  const vueShadowModuleCss: string[] = [];
  let vueShadowUnverifiable = vueGraph.unresolvedStyleImport;
  for (const [path, source] of styleSources) {
    if (pathOwners.get(path) !== packageRoot) continue;
    try {
      if (stylesheetAnalysis(path, source).selectorsUnverifiable) {
        vueShadowUnverifiable = true;
        continue;
      }
    } catch {
      vueShadowUnverifiable = true;
      continue;
    }
    // Module class and id names are localized at build time; the planner
    // indexes only their global (type/attribute/:global) selector surface.
    if (isStylesheetModule(path)) vueShadowModuleCss.push(source);
    else vueShadowCss.push({ path, source });
  }
  for (const file of sourceFiles) {
    if (extname(file.path) === ".html" && pathOwners.get(file.path) === packageRoot) {
      try {
        if (parseHtmlSource(file.path, file.source).hasStyle) vueShadowUnverifiable = true;
      } catch {
        vueShadowUnverifiable = true;
      }
    }
  }
  for (const file of ownedVue) {
    const analysis = analysisOf(file.path);
    if (analysis.retained) {
      if (analysis.styleRanges.length > 0) vueShadowUnverifiable = true;
      continue;
    }
    if (analysis.escapeUnverifiable || analysis.scriptImportsUnverifiable) {
      vueShadowUnverifiable = true;
    }
    for (const text of analysis.shadowPreprocessorTexts) {
      vueShadowCss.push({ path: file.path, source: text });
    }
    for (const text of analysis.unscopedShadowPreprocessorTexts) {
      vueShadowCss.push({ path: file.path, source: text, migratingUnscoped: true });
    }
    vueShadowCss.push(
      ...analysis.shadowCssTexts.map((source) => ({ path: file.path, source })),
      ...analysis.unscopedShadowCssTexts.map((source) => ({
        path: file.path,
        source,
        migratingUnscoped: true,
      })),
      ...analysis.blocks.map((block) => ({
        path: file.path,
        source: block.shadowSource ?? block.analysisSource ?? block.content,
        scoped: true,
      })),
    );
    vueShadowModuleCss.push(
      ...analysis.shadowModuleCssTexts,
      ...analysis.moduleBlocks.map((block) => block.analysisSource ?? block.content),
    );
  }

  const packageIsPrivate =
    JSON.parse(await snapshotFile(snapshots, join(packageRoot, "package.json"))).private === true;
  const files = new Map<string, PreparedSourceFile>();
  const stylesheets: StylesheetEntry[] = [];
  const stylePaths = new Set<string>();
  const unscopedPaths = new Set<string>();
  const ownedSources = sourceFiles.filter((file) => pathOwners.get(file.path) === packageRoot);
  // A scanned HTML file without a stylesheet link never enters sourceFiles,
  // but its class attributes are still a global usage surface (e.g. a
  // gitignored mount shell), so its existence alone defeats the proof.
  const sourcePathSet = new Set(sourceFiles.map((file) => file.path));
  const hiddenHtml = scannedPaths.some(
    (path) =>
      extname(path) === ".html" && pathOwners.get(path) === packageRoot && !sourcePathSet.has(path),
  );
  const projectWideUsageProven =
    packageIsPrivate &&
    workspaceRoot === packageRoot &&
    !workspaces &&
    !explicitStyle &&
    !hiddenHtml &&
    ownedSources.every((file) => targetable.has(file.path)) &&
    ownedVue.every((file) => !analysisOf(file.path).retained);
  const elementsByFile = new Map<string, VuePlannedElement[]>();
  const addElement = (
    path: string,
    element: VueTemplateSite & { matchIds?: string[]; matchTag?: string },
    cssPaths: string[],
  ): void => {
    if ((!element.classAttribute && !element.moduleBinding) || cssPaths.length === 0) return;
    const elements = elementsByFile.get(path) ?? [];
    elements.push({ ...element, cssPaths: [...new Set(cssPaths)] });
    elementsByFile.set(path, elements);
  };
  const elementClasses = (element: VueTemplateSite): string[] =>
    element.matchClasses ?? element.classAttribute?.value.split(/\s+/).filter(Boolean) ?? [];
  const componentRootSite = (site: VueTemplateSite, root: VueTemplateSite) => {
    const matchId = site.idAttribute?.value ?? root.idAttribute?.value;
    return {
      ...site,
      matchClasses: [...elementClasses(site), ...elementClasses(root)],
      matchIds: matchId ? [matchId] : [],
      matchTag: root.tag,
    };
  };

  for (const file of selected) {
    const analysis = analysisOf(file.path);
    if (analysis.retained) continue;
    const importedStyles = new Set(
      [
        // A bare script specifier is a package import; resolving it against
        // the SFC directory could bind an unrelated local file.
        ...analysis.scriptStyleImports
          .filter(
            (reference) =>
              reference.startsWith(".") &&
              STYLESHEET_SYNTAX.has(extname(reference.split(/[?#]/, 1)[0])),
          )
          .flatMap((reference) => stylesheetReferenceTargets(file.path, reference, styleSources)),
        ...analysis.styleBlockImports.flatMap((entry) =>
          stylesheetReferenceTargets(file.path, entry.reference, styleSources),
        ),
      ].filter((path) => pathOwners.get(path) === packageRoot),
    );
    for (const path of importedStyles) {
      stylePaths.add(path);
      if (extname(path) !== ".css") continue;
      for (const imported of cssImports(path, styleSources.get(path) ?? "")) {
        // Conditional imports need variant-aware contexts; retaining them is
        // safer than attaching an unconditional utility.
        if (imported.media) continue;
        for (const target of stylesheetReferenceTargets(path, imported.href, styleSources)) {
          if (pathOwners.get(target) === packageRoot) importedStyles.add(target);
        }
      }
    }
    const ownContexts = [
      ...(analysis.blocks.length > 0 ||
      analysis.unscopedBlocks.length > 0 ||
      (analysis.moduleBlocks.length > 0 && !analysis.moduleClosureBroken)
        ? [file.path]
        : []),
      ...[...importedStyles].filter((path) => !isStylesheetModule(path)),
    ];
    for (const element of analysis.htmlElements) {
      const ownElement =
        analysis.rootIdsOverridden && analysis.rootStarts.includes(element.nodeStart)
          ? { ...element, matchIds: [] }
          : element;
      addElement(file.path, ownElement, ownContexts);
    }
  }

  for (const parent of ownedVue) {
    const analysis = analysisOf(parent.path);
    if (analysis.retained) continue;
    for (const edge of analysis.resolvedComponents ?? []) {
      const childAnalysis = analyses.get(edge.child);
      const childDetails = childAnalysis && !childAnalysis.retained ? childAnalysis : undefined;
      // Vue only inherits call-site attributes onto a single-root child, so
      // the total root count must gate the rewrite independently of writable sites.
      const singleRoot =
        childDetails?.rootStarts.length === 1 &&
        !childDetails.rootVFor &&
        !childDetails.rootFragment;
      const childRoots = childDetails?.htmlElements.filter((element) =>
        childDetails.rootStarts.includes(element.nodeStart),
      );
      if (
        selectedPaths.has(parent.path) &&
        singleRoot &&
        childRoots?.length === 1 &&
        !childDetails?.fallthroughUnverifiable
      ) {
        addElement(parent.path, componentRootSite(edge.site, childRoots[0]), [parent.path]);
      }
      if (
        selectedPaths.has(edge.child) &&
        singleRoot &&
        // A self-recursive edge is already covered by the parent-style site
        // above; adding the same span twice would produce overlapping edits.
        edge.parent !== edge.child &&
        !childDetails?.fallthroughUnverifiable
      ) {
        addElement(parent.path, edge.site, [edge.child]);
        if (childRoots?.length === 1) {
          // The caller's classes land on the child root, so module planning
          // must see them next to the root's binding; the read-only class
          // attribute marks the record as shadow-only (never rewritten, never
          // counted as a reference).
          const shadowSite = {
            ...componentRootSite(edge.site, childRoots[0]),
            moduleBinding: childRoots[0].moduleBinding,
          };
          if (shadowSite.classAttribute) {
            shadowSite.classAttribute = { ...shadowSite.classAttribute, writable: false };
            addElement(parent.path, shadowSite, [edge.child]);
          }
        }
      }
    }
  }

  for (const file of selected) {
    await rejectSymlinkTarget(file.path, packageRoot);
    const analysis = analysisOf(file.path);
    warnings.push(...analysis.warnings);
    if (analysis.retained) continue;
    const componentOpen =
      analysis.componentsOpen ||
      (analysis.resolvedComponents ?? []).some((edge) => {
        const child = analyses.get(edge.child);
        return (
          !child ||
          child.retained ||
          child.dynamic ||
          child.fallthroughUnverifiable ||
          child.rootVFor ||
          child.rootFragment ||
          child.rootStarts.length !== 1 ||
          !child.htmlElements.some((element) => element.nodeStart === child.rootStarts[0])
        );
      });
    const callers = vueGraph.callers.get(file.path) ?? [];
    const callerOpen =
      !packageIsPrivate ||
      callers.length === 0 ||
      vueGraph.callerOpen.has(file.path) ||
      callers.some((edge) => {
        const parent = analyses.get(edge.parent);
        return parent ? parent.retained || parent.dynamic : false;
      });
    const retentionReason = (open: boolean): string | undefined => {
      if (analysis.dynamic) return "dynamic-template-class";
      if (open) return "component-class-target";
      if (analysis.alwaysRenderedRoots < 2 && callerOpen) return "open-root-fallthrough";
      return undefined;
    };
    const retention = retentionReason(componentOpen);
    // ponytail: unscoped deletion is limited to private single-source packages;
    // widen it only when the dependency graph can prove stylesheet co-loading.
    const migrateUnscoped =
      analysis.blocks.length === 0 &&
      analysis.unscopedBlocks.length > 0 &&
      projectWideUsageProven &&
      !analysis.dynamic &&
      // Injected and slotted content can use any class an unscoped rule
      // targets without ever receiving its utility.
      !analysis.vHtml &&
      !analysis.hasSlot &&
      !analysis.escapeUnverifiable &&
      !componentOpen &&
      ownedSources.length === 1 &&
      ownedSources[0].path === file.path;
    if (migrateUnscoped) {
      await compileBlocks(file, analysis.unscopedBlocks);
      unscopedPaths.add(file.path);
    }
    if (analysis.unscopedBlocks.length > 0 && !migrateUnscoped) {
      for (const block of analysis.unscopedBlocks) {
        warnings.push({
          code: "unscoped-style-block",
          file: file.path,
          start: block.contentStart,
          end: block.contentEnd,
          message: "The global reach of this unscoped style block could not be proven.",
        });
      }
    }
    // Module classes are hashed, so corpus CSS cannot target them directly --
    // but unknown classes can still land on a binding element (a dynamic
    // surface anywhere in the template, or parent fallthrough onto a single
    // root) and compete for the same properties, so those surfaces must be
    // closed exactly like the scoped ones. `componentOpen` is exempt: a child
    // component's root can never carry the hashed class, because a `$style`
    // binding on a component tag is already an opaque (dynamic) surface.
    const moduleRetention = retentionReason(false);
    const migrateModule =
      analysis.moduleBlocks.length > 0 && !analysis.moduleClosureBroken && !moduleRetention;
    if (analysis.moduleBlocks.length > 0 && !migrateModule) {
      const [code, message] = analysis.moduleClosureBroken
        ? [
            "unsupported-css-module-reference",
            "`$style` is referenced outside provable direct member accesses, so the module is retained.",
          ]
        : moduleRetention === "dynamic-template-class"
          ? [
              "dynamic-template-class",
              "A dynamic class binding makes the template's class set unprovable, so the module is retained.",
            ]
          : [
              "open-root-fallthrough",
              "A parent component can merge classes onto the single root element, so the module is retained.",
            ];
      for (const block of analysis.moduleBlocks) {
        warnings.push({
          code,
          file: file.path,
          start: block.contentStart,
          end: block.contentEnd,
          message,
        });
      }
    }
    // Module blocks were already compiled alongside the scoped blocks for
    // the shadow corpus; recompiling here would double the preprocessor work.
    const vueBlocks = migrateUnscoped ? analysis.unscopedBlocks : analysis.blocks;
    if (vueBlocks.length === 0 && !migrateModule) continue;
    const vueRetention = migrateUnscoped ? undefined : retention;
    // Same-file scoped blocks stay out of the general corpus; the batch planner
    // adds back only their retained selectors when it plans the module entry.
    const shadowCss = vueShadowCss
      .filter(
        (entry) =>
          entry.path !== file.path ||
          (!entry.scoped && !(migrateUnscoped && entry.migratingUnscoped)),
      )
      .map((entry) => entry.source);
    if (migrateModule) {
      // Module classes are hashed, so this entry is always a closed world;
      // its own block text remains in the module shadow channel, which can
      // only over-retain. It plans before the scoped entry so scoped
      // planning can see which `$style` bindings survived: a live binding
      // is an opaque cascade surface (its retained module rule competes
      // unlayered with a replacement utility).
      stylesheets.push({
        cssPath: file.path,
        cssSource: file.source,
        cssModuleId: normalizedRelativePath(workspaceRoot, file.path),
        syntax: "css",
        isModule: true,
        vueBlocks: analysis.moduleBlocks,
        vueModule: true,
        vueShadowCss: shadowCss,
        vueShadowModuleCss,
        vueShadowUnverifiable,
      });
    }
    if (vueBlocks.length > 0) {
      stylesheets.push({
        cssPath: file.path,
        cssSource: file.source,
        cssModuleId: normalizedRelativePath(workspaceRoot, file.path),
        syntax: "css",
        isModule: !vueRetention,
        vueBlocks,
        vueRetention,
        vueUnscoped: migrateUnscoped,
        vueShadowCss: vueRetention ? undefined : shadowCss,
        vueShadowModuleCss: vueRetention ? undefined : vueShadowModuleCss,
        vueShadowUnverifiable: vueRetention ? undefined : vueShadowUnverifiable,
      });
    }
  }

  for (const file of ownedVue) {
    const analysis = analysisOf(file.path);
    if (analysis.retained) continue;
    const elements = elementsByFile.get(file.path) ?? [];
    const contextPaths = [...new Set(elements.flatMap((element) => element.cssPaths))];
    if (contextPaths.length > 0) await rejectSymlinkTarget(file.path, packageRoot);
    files.set(file.path, {
      ...file,
      ...(contextPaths.length > 0
        ? {
            htmlElements: elements,
            htmlStylesheets: contextPaths.map((cssPath) => ({
              cssPath,
              variants: [],
              direct: cssPath === file.path,
              analyzable: true,
            })),
            htmlReferencesSafe: !analysis.dynamic,
            htmlScriptText: analysis.scriptText,
          }
        : {}),
      sourceImports: analysis.scriptImports,
      sourceImportsUnverifiable: analysis.scriptImportsUnverifiable,
    });
  }
  return {
    files,
    stylesheets,
    styleRanges: new Map(
      [...analyses].map(([path, analysis]) => [path, analysis.styleRanges] as const),
    ),
    stylePaths,
    unscopedPaths,
    warnings,
    compiler,
  };
}
