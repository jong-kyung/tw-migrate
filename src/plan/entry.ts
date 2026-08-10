// Workspace shared-entry resolution: the entry catalog and the dual proofs
// that let a package without its own Tailwind entry reuse an ancestor's.
//
// Package ancestry identifies candidates but never proves consumption.
// Selecting an ancestor entry requires a loading proof (a child-owned
// writable source imports or links the entry, and every consumer of a
// migrated stylesheet is statically reachable from such a source through
// child-owned imports) and a scan proof (the entry's utility detection
// provably covers the child package). A stylesheet with a consumer outside
// every proven flow keeps its rules retained while the rest of the package
// migrates.

import { createRequire } from "node:module";
import { dirname, extname, join, resolve } from "node:path";

import { collectCssDirectives, collectSourceImports } from "../native.ts";
import { parseHtmlSource } from "../parser/html.ts";
import { isWithin, maskCssComments } from "../util/shared.ts";
import type { PreparedSourceFile } from "../types.ts";

const TAILWIND_UTILITIES_IMPORT = /@import\s+["']tailwindcss(?:\/utilities(?:\.css)?)?["']/;

/// True for a specifier whose import actually emits utilities: the full
/// package or its utilities layer. A sheet importing only
/// `tailwindcss/theme` or `tailwindcss/preflight` produces no utility
/// CSS, so selecting it as an entry would migrate classes that never
/// compile into styles.
function emitsUtilities(specifier: string): boolean {
  return (
    specifier === "tailwindcss" ||
    specifier === "tailwindcss/utilities" ||
    specifier === "tailwindcss/utilities.css"
  );
}

/// A structurally parsed top-level Tailwind import that includes the
/// utilities layer. An unparseable stylesheet falls back to the masked
/// regex so a working entry is never dropped from discovery.
function hasTailwindImport(source: string): boolean {
  const directives = cssDirectives(source);
  if (directives !== null) {
    return directives.some(
      (directive) =>
        directive.kind === "import" &&
        directive.specifier !== null &&
        emitsUtilities(directive.specifier),
    );
  }
  return TAILWIND_UTILITIES_IMPORT.test(maskCssComments(source));
}

/// Tailwind entries per owning package, from the scanned stylesheet corpus.
export function tailwindEntryCatalog(
  styleSources: Map<string, string>,
  pathOwners: Map<string, string | undefined>,
): Map<string, string[]> {
  const catalog = new Map<string, string[]>();
  for (const [path, source] of styleSources) {
    if (extname(path) !== ".css") continue;
    if (!hasTailwindImport(source)) continue;
    const owner = pathOwners.get(path);
    if (owner === undefined) continue;
    const entries = catalog.get(owner) ?? [];
    entries.push(path);
    catalog.set(owner, entries);
  }
  for (const entries of catalog.values()) entries.sort();
  return catalog;
}

type CssDirective =
  | {
      kind: "import";
      specifier: string | null;
      tailwind: boolean;
      source: string | null;
      sourceUnreadable: boolean;
    }
  | { kind: "source"; not: boolean; inline: boolean; scope: string | null; unreadable: boolean }
  | { kind: "other"; name: string };

/// Parsed top-level at-rule directives of one stylesheet through the
/// native oxc-css-parser, or null when the stylesheet does not parse.
/// Tailwind honors `@source` and `@import` only at the top level, so
/// directive-shaped text inside rule blocks or string values never
/// counts, and import targets and source modifiers arrive structurally
/// instead of through text matching.
function cssDirectives(source: string): CssDirective[] | null {
  try {
    const parsed: unknown = JSON.parse(collectCssDirectives(source));
    if (!Array.isArray(parsed)) return null;
    return parsed.filter(
      (directive): directive is CssDirective =>
        directive !== null && typeof directive === "object" && typeof directive.kind === "string",
    );
  } catch {
    return null;
  }
}

/// The entry's static import graph over the scanned stylesheet corpus:
/// Tailwind processes source directives from imported project CSS as part
/// of one entry graph. Returns null when a local import is missing from
/// the corpus, does not parse, or has an unreadable target, because its
/// directives are unknowable.
function entryGraphSheets(
  entry: string,
  styleSources: Map<string, string>,
): { path: string; directives: CssDirective[] }[] | null {
  const sheets: { path: string; directives: CssDirective[] }[] = [];
  const seen = new Set<string>();
  const pending = [entry];
  while (pending.length > 0) {
    const path = pending.pop();
    if (path === undefined || seen.has(path)) continue;
    seen.add(path);
    const raw = styleSources.get(path);
    if (raw === undefined) return null;
    const directives = cssDirectives(raw);
    if (directives === null) return null;
    sheets.push({ path, directives });
    for (const directive of directives) {
      if (directive.kind !== "import") continue;
      if (directive.specifier === null) return null;
      const spec = directive.specifier;
      if (directive.tailwind) continue;
      if (!spec.startsWith(".") && !spec.startsWith("/")) {
        // Bare package imports load through the same resolver as the
        // Tailwind loader. A workspace-internal target joins the graph; a
        // dependency under node_modules cannot target the workspace with
        // its relative scopes; an unresolvable one leaves the graph's
        // directives unknowable.
        let resolved: string;
        try {
          resolved = createRequire(join(dirname(path), "package.json")).resolve(spec);
        } catch {
          return null;
        }
        if (/[\\/]node_modules[\\/]/.test(resolved)) continue;
        pending.push(resolved);
        continue;
      }
      const resolved = resolve(dirname(path), spec);
      pending.push(styleSources.has(resolved) ? resolved : `${resolved}.css`);
    }
  }
  return sheets;
}

/// How the entry's utility detection covers the child package, or null when
/// coverage cannot be proven. Source directives are evaluated across the
/// entry's imported graph, each resolved relative to its owning
/// stylesheet. A literal `@source` scope is scanned regardless of ignore
/// rules; automatic detection additionally requires each consumer path to
/// pass the effective scanner ignore rules.
export function scanProof(options: {
  entry: string;
  entrySource: string;
  packageRoot: string;
  styleSources?: Map<string, string>;
}): "literal" | "automatic" | null {
  const sheets = entryGraphSheets(
    options.entry,
    new Map([...(options.styleSources ?? []), [options.entry, options.entrySource]]),
  );
  if (sheets === null) return null;

  let literal = false;
  let automaticDisabled = false;
  for (const { path, directives } of sheets) {
    const base = dirname(path);
    // A literal scope proves coverage only when it contains the whole child
    // package; a narrower scope, or one truncated mid-segment by a
    // wildcard, proves nothing for consumers outside it and falls through
    // to the automatic-detection arm.
    const coversChild = (scope: string): boolean => {
      // A wildcard scope may restrict file types or depth, so it never
      // proves package-wide coverage; only a plain directory scope does.
      if (scope.includes("*")) return false;
      return isWithin(resolve(base, scope), options.packageRoot);
    };
    // An exclusion overlapping the child in either direction defeats both
    // proofs conservatively; a wildcard exclusion matches by string prefix
    // because `./packages/app*` excludes trees no path-segment comparison
    // sees.
    const excludesChild = (scope: string): boolean => {
      const stripped = resolve(base, scope.replace(/\*.*$/, ""));
      if (scope.includes("*")) {
        return options.packageRoot.startsWith(stripped) || stripped.startsWith(options.packageRoot);
      }
      return isWithin(stripped, options.packageRoot) || isWithin(options.packageRoot, stripped);
    };
    for (const directive of directives) {
      if (directive.kind === "source") {
        if (directive.inline) continue;
        // An unreadable scope leaves coverage or exclusion unknowable.
        if (directive.unreadable || directive.scope === null) return null;
        if (directive.not) {
          if (excludesChild(directive.scope)) return null;
        } else if (coversChild(directive.scope)) {
          literal = true;
        }
      } else if (directive.kind === "import" && directive.tailwind) {
        if (directive.sourceUnreadable) return null;
        if (directive.source === "none") automaticDisabled = true;
        else if (directive.source !== null) {
          if (coversChild(directive.source)) literal = true;
          else automaticDisabled = true;
        }
      }
    }
  }
  if (literal) return "literal";
  if (automaticDisabled) return null;
  return isWithin(dirname(options.entry), options.packageRoot) ? "automatic" : null;
}

interface SourceImportRecord {
  specifier: string;
  typeOnly: boolean;
  dynamic: boolean;
}

const importRecordCache = new Map<string, { source: string; records: SourceImportRecord[] }>();

/// Parsed module records of one source file through the native oxc parser.
/// Vue SFC scripts are extracted first; a file that does not parse has no
/// provable imports, which only makes proofs fail conservatively. Records
/// are memoized by path and content because every package's proofs scan
/// the workspace corpus.
function sourceImports(file: { path: string; source: string }): SourceImportRecord[] {
  const cached = importRecordCache.get(file.path);
  if (cached !== undefined && cached.source === file.source) return cached.records;
  const extension = extname(file.path);
  if (extension === ".html") return [];
  const sources =
    extension === ".vue"
      ? [
          // A commented-out script block never executes, so it must not
          // contribute loader or reachability records.
          ...file.source
            .replace(/<!--[\s\S]*?-->/g, "")
            .matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/gi),
        ].map((match) => {
          // Parse each block with its declared language so JSX blocks are
          // not rejected by the TypeScript grammar.
          const lang = match[1].match(/\blang\s*=\s*["']?(\w+)/)?.[1]?.toLowerCase();
          const scriptExtension =
            lang === "tsx" || lang === "jsx" || lang === "js" ? `.${lang}` : ".ts";
          return { source: match[2], path: `${file.path}${scriptExtension}` };
        })
      : [{ source: file.source, path: file.path }];
  const records: SourceImportRecord[] = [];
  for (const script of sources) {
    try {
      const parsed: unknown = JSON.parse(collectSourceImports(script.source, script.path));
      if (!Array.isArray(parsed)) continue;
      for (const record of parsed) {
        if (
          record !== null &&
          typeof record === "object" &&
          typeof record.specifier === "string" &&
          typeof record.typeOnly === "boolean" &&
          typeof record.dynamic === "boolean"
        ) {
          records.push({
            specifier: record.specifier,
            typeOnly: record.typeOnly,
            dynamic: record.dynamic,
          });
        }
      }
    } catch {
      // Unparseable sources prove nothing.
    }
  }
  importRecordCache.set(file.path, { source: file.source, records });
  return records;
}

const RESOLVABLE_EXTENSIONS = [
  ".ts",
  ".tsx",
  ".mts",
  ".cts",
  ".js",
  ".jsx",
  ".mjs",
  ".cjs",
  ".vue",
];

/// Resolve a relative import specifier to a known child source path. A
/// NodeNext-style emitted specifier such as `./Button.js` also resolves to
/// its authored TypeScript counterpart.
function resolveImport(fromDir: string, spec: string, known: Set<string>): string | undefined {
  if (!spec.startsWith(".")) return undefined;
  const base = resolve(fromDir, spec);
  const emitted = base.match(/^(.*)\.(?:mjs|cjs|js|jsx)$/);
  const candidates = [
    base,
    ...(emitted
      ? [".ts", ".tsx", ".mts", ".cts"].map((extension) => `${emitted[1]}${extension}`)
      : []),
    ...RESOLVABLE_EXTENSIONS.map((extension) => `${base}${extension}`),
    ...RESOLVABLE_EXTENSIONS.map((extension) => join(base, `index${extension}`)),
  ];
  return candidates.find((candidate) => known.has(candidate));
}

/// A specifier that must resolve to a script source: an unresolved one in
/// the exposure walk hides part of the externally loadable closure.
function scriptLikeSpecifier(spec: string): boolean {
  if (!spec.startsWith(".")) return false;
  const trimmed = spec.split(/[?#]/, 1)[0];
  return /\.(?:m?[jt]sx?|c[jt]s|vue)$/.test(trimmed) || !/\.[^/.]+$/.test(trimmed);
}

/// A bare specifier shaped like an external package or builtin. Package
/// aliases such as `#internal` or `@/components` fall outside this shape
/// and may map back into package sources, so an unresolved one inside the
/// exposed closure is conservative. An alias styled exactly like a scoped
/// npm name remains indistinguishable from a real dependency.
function externalPackageSpecifier(spec: string): boolean {
  return /^(?:node:[\w/.-]+|(?:@[a-z0-9~][\w.~-]*\/)?[a-z0-9~][\w.~-]*(?:\/.*)?)$/.test(spec);
}

/// Child-owned static import edges. HTML files contribute no outgoing edges:
/// an HTML consumer proves only itself through its own entry link.
function importEdges(files: PreparedSourceFile[]): Map<string, string[]> {
  const known = new Set(files.map((file) => file.path));
  const edges = new Map<string, string[]>();
  for (const file of files) {
    if (extname(file.path) === ".html") continue;
    const targets: string[] = [];
    // Type-only records are erased at runtime and load nothing, so they
    // never carry loading reachability.
    for (const record of sourceImports(file)) {
      if (record.typeOnly) continue;
      const resolved = resolveImport(dirname(file.path), record.specifier, known);
      if (resolved !== undefined) targets.push(resolved);
    }
    edges.set(file.path, targets);
  }
  return edges;
}

function reachableFrom(starts: Iterable<string>, edges: Map<string, string[]>): Set<string> {
  const reached = new Set<string>(starts);
  const pending = [...reached];
  while (pending.length > 0) {
    const current = pending.pop();
    if (current === undefined) break;
    for (const target of edges.get(current) ?? []) {
      if (!reached.has(target)) {
        reached.add(target);
        pending.push(target);
      }
    }
  }
  return reached;
}

/// Files exposed through the child's package entry points. A consumer an
/// external application can import may run without this entry's CSS, so
/// exposure defeats loading proof regardless of in-repository reachability.
/// A wildcard exports pattern exposes everything.
function exposedFiles(
  packageRoot: string,
  packageJson: Record<string, unknown>,
  files: PreparedSourceFile[],
  inboundSeeds: string[],
): Set<string> | "all" {
  const known = new Set(files.map((file) => file.path));
  const specs: string[] = [];
  const collect = (value: unknown): "all" | undefined => {
    if (typeof value === "string") {
      if (value.includes("*")) return "all";
      specs.push(value);
      return undefined;
    }
    if (value !== null && typeof value === "object") {
      for (const [key, child] of Object.entries(value)) {
        if (key.includes("*")) return "all";
        if (collect(child) === "all") return "all";
      }
    }
    return undefined;
  };
  for (const field of ["main", "module", "exports", "browser"]) {
    if (collect(packageJson[field]) === "all") return "all";
  }
  // Without an encapsulating `exports` map a publishable package is open
  // to deep imports of any source subpath; a private package cannot be
  // installed externally and keeps conventional resolution only.
  if (packageJson.exports === undefined && packageJson.private !== true) return "all";
  const declared = specs.length > 0;
  const entryFileSeeds = [...inboundSeeds];
  // Without export metadata the conventional `./index` entry remains
  // externally loadable; when no index resolves, nothing is exposed by
  // convention either.
  if (!declared) specs.push("./index");
  const entryFiles: string[] = [...entryFileSeeds];
  for (const spec of specs) {
    const resolved = resolveImport(packageRoot, spec.startsWith(".") ? spec : `./${spec}`, known);
    // A declared entry point that does not resolve to a scanned source,
    // such as an unbuilt `./dist/index.js`, exposes an unknowable set of
    // sources, so the exposure proof turns conservative instead of
    // silently dropping the target. A missing conventional index simply
    // exposes nothing.
    if (resolved === undefined) {
      if (declared) return "all";
      continue;
    }
    entryFiles.push(resolved);
  }
  // The exposure walk must not silently drop edges: an unresolved
  // script-like import inside the exposed closure hides sources an
  // external application can still execute. Stylesheet and asset imports
  // resolve outside the source corpus by design and carry no exposure.
  const byPath = new Map(files.map((file) => [file.path, file]));
  const exposed = new Set<string>(entryFiles);
  const pending = [...entryFiles];
  while (pending.length > 0) {
    const current = pending.pop();
    if (current === undefined) break;
    const file = byPath.get(current);
    if (file === undefined || extname(file.path) === ".html") continue;
    for (const record of sourceImports(file)) {
      if (record.typeOnly) continue;
      const resolved = resolveImport(dirname(file.path), record.specifier, known);
      if (resolved === undefined) {
        if (
          scriptLikeSpecifier(record.specifier) ||
          (!record.specifier.startsWith(".") && !externalPackageSpecifier(record.specifier))
        ) {
          return "all";
        }
        continue;
      }
      if (!exposed.has(resolved)) {
        exposed.add(resolved);
        pending.push(resolved);
      }
    }
  }
  return exposed;
}

export interface SharedEntryProofOptions {
  packageRoot: string;
  entry: string;
  entrySource: string;
  /// The scanned stylesheet corpus, for resolving the entry's imported
  /// graph during the scan proof.
  styleSources?: Map<string, string>;
  /// The prepared sources, including HTML stylesheet contexts. Files not
  /// owned by the child are excluded from every proof graph.
  packageSources: PreparedSourceFile[];
  owned: (path: string) => boolean;
  writable: (path: string) => boolean;
  packageJson: Record<string, unknown>;
  /// Consumer paths excluded by the effective scanner ignore rules; only
  /// consulted when the scan proof relies on automatic detection.
  ignoredPaths: Set<string>;
}

export interface SharedEntryProofs {
  /// True when this consumer file sits inside a proven loading flow and
  /// the entry's detection scope covers it.
  provenConsumer: (consumer: PreparedSourceFile) => boolean;
  /// Stylesheets proven for every consumer keep migrating; a stylesheet
  /// with any unproven consumer flow is retained.
  provenStyle: (stylePath: string, consumers: PreparedSourceFile[]) => boolean;
}

/// Prove the dual shared-entry relationship for one candidate ancestor
/// entry, or return null when the child never provably loads it or its
/// detection scope cannot be shown to cover the child.
export function proveSharedEntry(options: SharedEntryProofOptions): SharedEntryProofs | null {
  const scan = scanProof(options);
  if (scan === null) return null;

  // Reachability is proven through child-owned imports only: a foreign
  // file can be executed by its owning package without loading this
  // entry, so it neither carries edges nor counts as a proven consumer.
  const ownedSources = options.packageSources.filter((file) => options.owned(file.path));
  const loaders = ownedSources
    .filter((file) => options.writable(file.path))
    .filter(
      (file) =>
        (file.htmlStylesheets ?? []).some((context) => context.cssPath === options.entry) ||
        htmlLinksEntry(file, options.entry) ||
        (extname(file.path) !== ".html" && importsEntry(file, options.entry)),
    )
    .map((file) => file.path);
  if (loaders.length === 0) return null;

  const edges = importEdges(ownedSources);
  const reachable = reachableFrom(loaders, edges);
  // A sibling workspace file can deep-import child sources directly, and
  // `private` prevents publication without encapsulating in-repository
  // subpaths, so whatever a foreign file imports is executable without
  // this entry and seeds the exposed closure.
  const ownedPaths = new Set(ownedSources.map((file) => file.path));
  const packageName =
    typeof options.packageJson.name === "string" ? options.packageJson.name : undefined;
  const resolveInbound = (fromDir: string, spec: string): string | undefined => {
    const relative = resolveImport(fromDir, spec, ownedPaths);
    if (relative !== undefined) return relative;
    // A sibling can also deep-import through the package name; the
    // subpath resolves against the package root.
    if (packageName !== undefined && spec.startsWith(`${packageName}/`)) {
      return resolveImport(
        options.packageRoot,
        `./${spec.slice(packageName.length + 1)}`,
        ownedPaths,
      );
    }
    return undefined;
  };
  const inboundSeeds: string[] = [];
  for (const file of options.packageSources) {
    if (options.owned(file.path) || extname(file.path) === ".html") continue;
    for (const record of sourceImports(file)) {
      if (record.typeOnly) continue;
      const resolved = resolveInbound(dirname(file.path), record.specifier);
      if (resolved !== undefined) inboundSeeds.push(resolved);
    }
  }
  const exposed = exposedFiles(
    options.packageRoot,
    options.packageJson,
    ownedSources,
    inboundSeeds,
  );

  const provenConsumer = (consumer: PreparedSourceFile): boolean =>
    options.owned(consumer.path) &&
    reachable.has(consumer.path) &&
    (exposed === "all" ? false : !exposed.has(consumer.path)) &&
    (scan === "literal" || !options.ignoredPaths.has(consumer.path));
  return {
    provenConsumer,
    // A stylesheet with no consumer at all has no proven flow either: its
    // movable at-rules would otherwise activate in the shared entry for
    // CSS nothing provably loads.
    provenStyle: (stylePath, consumers) => consumers.length > 0 && consumers.every(provenConsumer),
  };
}

/// An HTML stylesheet link resolving to the entry, through the real HTML
/// parser so commented-out markup never counts and entity-encoded hrefs
/// resolve. Prepared HTML contexts keep only package-local links, so
/// ancestor entry links are re-resolved here. A `<base>` element changes
/// href resolution and stays unproven, as does unparseable HTML.
function htmlLinksEntry(file: { path: string; source: string }, entry: string): boolean {
  if (extname(file.path) !== ".html") return false;
  try {
    const parsed = parseHtmlSource(file.path, file.source);
    if (parsed.bases.length > 0) return false;
    return parsed.links.some((link) => {
      // A media-conditioned entry link loads the CSS only while its query
      // matches, so it proves nothing unconditionally.
      const media = link.media.trim().toLowerCase();
      if (media !== "" && media !== "all") return false;
      const href = link.href.split(/[?#]/, 1)[0];
      if (!href || /^[a-z][a-z0-9+.-]*:|^\/\//i.test(href)) return false;
      return resolve(dirname(file.path), href) === entry;
    });
  } catch {
    return false;
  }
}

/// A parsed import whose specifier resolves to the stylesheet. Consumer
/// detection must not accept a path mentioned in a comment or unrelated
/// string: the native rewriter only edits parsed module references, so
/// the proof layer sees the same consumer set it does.
export function importsStylesheet(
  file: { path: string; source: string },
  stylePath: string,
): boolean {
  return sourceImports(file).some(
    (record) =>
      !record.typeOnly &&
      record.specifier.startsWith(".") &&
      resolve(dirname(file.path), record.specifier) === stylePath,
  );
}

/// A parsed static import declaration whose specifier resolves to the
/// entry. A loading proof needs an unconditional runtime import: comments,
/// unrelated strings, erased type-only clauses, and dynamic imports that
/// may sit behind control flow never prove that the entry's CSS loads.
function importsEntry(file: { path: string; source: string }, entry: string): boolean {
  return sourceImports(file).some(
    (record) =>
      !record.typeOnly &&
      !record.dynamic &&
      record.specifier.startsWith(".") &&
      resolve(dirname(file.path), record.specifier) === entry,
  );
}
