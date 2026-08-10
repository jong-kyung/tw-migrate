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

import { dirname, extname, join, resolve } from "node:path";

import { collectCssDirectives, collectSourceImports } from "../native.ts";
import { parseHtmlSource } from "../parser/html.ts";
import { isWithin, maskCssComments } from "../util/shared.ts";
import type { PreparedSourceFile } from "../types.ts";

const TAILWIND_IMPORT = /@import\s+["']tailwindcss(?:\/[^"']*)?["']/;

/// Tailwind entries per owning package, from the scanned stylesheet corpus.
export function tailwindEntryCatalog(
  styleSources: Map<string, string>,
  pathOwners: Map<string, string | undefined>,
): Map<string, string[]> {
  const catalog = new Map<string, string[]>();
  for (const [path, source] of styleSources) {
    if (extname(path) !== ".css") continue;
    if (!TAILWIND_IMPORT.test(maskCssComments(source))) continue;
    const owner = pathOwners.get(path);
    if (owner === undefined) continue;
    const entries = catalog.get(owner) ?? [];
    entries.push(path);
    catalog.set(owner, entries);
  }
  for (const entries of catalog.values()) entries.sort();
  return catalog;
}

/// Parsed top-level at-rule directives of one stylesheet, or null when the
/// stylesheet does not parse. Tailwind honors `@source` and `@import` only
/// at the top level, so directive-shaped text inside rule blocks or string
/// values never counts.
function cssDirectives(source: string): { name: string; text: string }[] | null {
  try {
    const parsed: unknown = JSON.parse(collectCssDirectives(source));
    if (!Array.isArray(parsed)) return null;
    return parsed.filter(
      (directive): directive is { name: string; text: string } =>
        directive !== null &&
        typeof directive === "object" &&
        typeof directive.name === "string" &&
        typeof directive.text === "string",
    );
  } catch {
    return null;
  }
}

/// The entry's static import graph over the scanned stylesheet corpus:
/// Tailwind processes source directives from imported project CSS as part
/// of one entry graph. Returns null when a local import is missing from
/// the corpus or does not parse, because its directives are unknowable.
function entryGraphSheets(
  entry: string,
  styleSources: Map<string, string>,
): { path: string; directives: { name: string; text: string }[] }[] | null {
  const sheets: { path: string; directives: { name: string; text: string }[] }[] = [];
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
      if (directive.name !== "import") continue;
      const spec = directive.text.match(/@import\s+(?:url\(\s*)?["']([^"']+)["']/)?.[1];
      if (spec === undefined || (!spec.startsWith(".") && !spec.startsWith("/"))) continue;
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
    const scopePath = (scope: string): string => resolve(base, scope.replace(/[/\\]\*.*$/, ""));
    // A literal scope proves coverage only when it contains the whole child
    // package; a narrower scope proves nothing for consumers outside it, so
    // it falls through to the automatic-detection arm.
    const coversChild = (scope: string): boolean => isWithin(scopePath(scope), options.packageRoot);
    // An exclusion overlapping the child in either direction defeats both
    // proofs conservatively.
    const excludesChild = (scope: string): boolean =>
      isWithin(scopePath(scope), options.packageRoot) ||
      isWithin(options.packageRoot, scopePath(scope));
    for (const { name, text } of directives) {
      if (name === "source") {
        const excluded = text.match(/@source\s+not\s+["']([^"']+)["']/);
        if (excluded && excludesChild(excluded[1])) return null;
        const scope = excluded ? undefined : text.match(/@source\s+["']([^"']+)["']/);
        if (scope && coversChild(scope[1])) literal = true;
      } else if (name === "import") {
        const importSource = text.match(/@import\s+["']tailwindcss["'][^;]*\bsource\(([^)]*)\)/);
        if (!importSource) continue;
        const argument = importSource[1].trim();
        if (argument === "none") automaticDisabled = true;
        else {
          const literalBase = argument.match(/^["']([^"']+)["']$/);
          if (literalBase && coversChild(literalBase[1])) literal = true;
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
}

/// Parsed module records of one source file through the native oxc parser.
/// Vue SFC scripts are extracted first; a file that does not parse has no
/// provable imports, which only makes proofs fail conservatively.
function sourceImports(file: { path: string; source: string }): SourceImportRecord[] {
  const extension = extname(file.path);
  if (extension === ".html") return [];
  const sources =
    extension === ".vue"
      ? [...file.source.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)].map((match) => match[1])
      : [file.source];
  const records: SourceImportRecord[] = [];
  for (const source of sources) {
    try {
      const parsed: unknown = JSON.parse(
        collectSourceImports(source, extension === ".vue" ? `${file.path}.ts` : file.path),
      );
      if (!Array.isArray(parsed)) continue;
      for (const record of parsed) {
        if (
          record !== null &&
          typeof record === "object" &&
          typeof record.specifier === "string" &&
          typeof record.typeOnly === "boolean"
        ) {
          records.push({ specifier: record.specifier, typeOnly: record.typeOnly });
        }
      }
    } catch {
      // Unparseable sources prove nothing.
    }
  }
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

/// Resolve a relative import specifier to a known child source path.
function resolveImport(fromDir: string, spec: string, known: Set<string>): string | undefined {
  if (!spec.startsWith(".")) return undefined;
  const base = resolve(fromDir, spec);
  const candidates = [
    base,
    ...RESOLVABLE_EXTENSIONS.map((extension) => `${base}${extension}`),
    ...RESOLVABLE_EXTENSIONS.map((extension) => join(base, `index${extension}`)),
  ];
  return candidates.find((candidate) => known.has(candidate));
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
  for (const field of ["main", "module", "exports"]) {
    if (collect(packageJson[field]) === "all") return "all";
  }
  const declared = specs.length > 0;
  // Without export metadata the conventional `./index` entry remains
  // externally loadable; when no index resolves, nothing is exposed by
  // convention either.
  if (!declared) specs.push("./index");
  const entryFiles: string[] = [];
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
  return reachableFrom(entryFiles, importEdges(files));
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
  const exposed = exposedFiles(options.packageRoot, options.packageJson, ownedSources);

  return {
    provenStyle: (stylePath, consumers) =>
      consumers.every(
        (consumer) =>
          options.owned(consumer.path) &&
          reachable.has(consumer.path) &&
          (exposed === "all" ? false : !exposed.has(consumer.path)) &&
          (scan === "literal" || !options.ignoredPaths.has(consumer.path)),
      ),
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
      const href = link.href.split(/[?#]/, 1)[0];
      if (!href || /^[a-z][a-z0-9+.-]*:|^\/\//i.test(href)) return false;
      return resolve(dirname(file.path), href) === entry;
    });
  } catch {
    return false;
  }
}

/// A parsed runtime import, dynamic import, or require whose specifier
/// resolves to the entry. A loading proof needs an actual runtime import:
/// a quoted path in a comment or unrelated string, and a type-only clause
/// TypeScript erases, never load the entry's CSS.
function importsEntry(file: { path: string; source: string }, entry: string): boolean {
  return sourceImports(file).some(
    (record) =>
      !record.typeOnly &&
      record.specifier.startsWith(".") &&
      resolve(dirname(file.path), record.specifier) === entry,
  );
}
