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

/// The entry's static import graph over the scanned stylesheet corpus:
/// Tailwind processes source directives from imported project CSS as part
/// of one entry graph. Returns null when a local import is missing from
/// the corpus, because its directives are unknowable.
function entryGraphSheets(
  entry: string,
  styleSources: Map<string, string>,
): { path: string; source: string }[] | null {
  const sheets: { path: string; source: string }[] = [];
  const seen = new Set<string>();
  const pending = [entry];
  while (pending.length > 0) {
    const path = pending.pop();
    if (path === undefined || seen.has(path)) continue;
    seen.add(path);
    const raw = styleSources.get(path);
    if (raw === undefined) return null;
    const source = maskCssComments(raw);
    sheets.push({ path, source });
    for (const match of source.matchAll(/@import\s+(?:url\(\s*)?["']([^"']+)["']/g)) {
      const spec = match[1];
      if (!spec.startsWith(".") && !spec.startsWith("/")) continue;
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
  for (const { path, source } of sheets) {
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
    for (const match of source.matchAll(/@source\s+not\s+["']([^"']+)["']/g)) {
      if (excludesChild(match[1])) return null;
    }
    for (const match of source.matchAll(/@source\s+["']([^"']+)["']/g)) {
      if (coversChild(match[1])) literal = true;
    }
    const importSource = source.match(/@import\s+["']tailwindcss["'][^;]*\bsource\(([^)]*)\)/);
    if (importSource) {
      const argument = importSource[1].trim();
      if (argument === "none") automaticDisabled = true;
      else {
        const literalBase = argument.match(/^["']([^"']+)["']$/);
        if (literalBase && coversChild(literalBase[1])) literal = true;
        else automaticDisabled = true;
      }
    }
  }
  if (literal) return "literal";
  if (automaticDisabled) return null;
  return isWithin(dirname(options.entry), options.packageRoot) ? "automatic" : null;
}

/// Replace JS comments with spaces so commented-out imports never create
/// loader or reachability edges. String and template contexts are tracked
/// so comment markers inside literals stay untouched. Regex literals and
/// `${}` re-entry inside templates are not modeled; mis-masking there can
/// only remove candidate matches, which makes proofs fail conservatively.
function maskJsComments(source: string): string {
  // Split into UTF-16 units so writes align with the index-based scan.
  const output = source.split("");
  type State = "code" | "single" | "double" | "template" | "line" | "block";
  let state: State = "code";
  for (let index = 0; index < source.length; index += 1) {
    const char = source[index];
    const next = source[index + 1];
    switch (state) {
      case "code":
        if (char === "/" && next === "/") {
          state = "line";
          output[index] = " ";
        } else if (char === "/" && next === "*") {
          state = "block";
          output[index] = " ";
        } else if (char === "'") state = "single";
        else if (char === '"') state = "double";
        else if (char === "`") state = "template";
        break;
      case "single":
        if (char === "\\") index += 1;
        else if (char === "'" || char === "\n") state = "code";
        break;
      case "double":
        if (char === "\\") index += 1;
        else if (char === '"' || char === "\n") state = "code";
        break;
      case "template":
        if (char === "\\") index += 1;
        else if (char === "`") state = "code";
        break;
      case "line":
        if (char === "\n") state = "code";
        else output[index] = " ";
        break;
      case "block":
        if (char === "*" && next === "/") {
          output[index] = " ";
          output[index + 1] = " ";
          index += 1;
          state = "code";
        } else if (char !== "\n") output[index] = " ";
        break;
    }
  }
  return output.join("");
}

const IMPORT_SPECIFIERS =
  /(?:\bimport|\bexport)\s+(?:[\w$*{},\s]+?from\s+)?["']([^"']+)["']|\bimport\s*\(\s*["']([^"']+)["']\s*\)|\brequire\s*\(\s*["']([^"']+)["']\s*\)/g;

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
    for (const match of maskJsComments(file.source).matchAll(IMPORT_SPECIFIERS)) {
      // Type-only imports are erased at runtime and load nothing, so they
      // never carry loading reachability. A clause whose specifiers are
      // all inline `type` entries is excluded the same way; over-excluding
      // a mixed default-and-type clause only makes the proof fail
      // conservatively.
      if (/^(?:import|export)\s+type\b/.test(match[0])) continue;
      const braces = match[0].match(/\{([^}]*)\}/);
      if (braces && braces[1].split(",").every((part) => /^\s*type\s/.test(part))) continue;
      const spec = match[1] ?? match[2] ?? match[3];
      const resolved = resolveImport(dirname(file.path), spec, known);
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
  const entryFiles: string[] = [];
  for (const spec of specs) {
    const resolved = resolveImport(packageRoot, spec.startsWith(".") ? spec : `./${spec}`, known);
    // A declared entry point that does not resolve to a scanned source,
    // such as an unbuilt `./dist/index.js`, exposes an unknowable set of
    // sources, so the exposure proof turns conservative instead of
    // silently dropping the target.
    if (resolved === undefined) return "all";
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

/// An HTML stylesheet link resolving to the entry. Prepared HTML contexts
/// keep only package-local links, so ancestor entry links are re-resolved
/// here from the raw source. A `<base>` tag changes href resolution and an
/// entity-encoded href is invisible to this scan; both stay unproven.
function htmlLinksEntry(file: { path: string; source: string }, entry: string): boolean {
  if (extname(file.path) !== ".html") return false;
  if (/<base\b/i.test(file.source)) return false;
  for (const match of file.source.matchAll(/<link\b[^>]*\bhref\s*=\s*["']([^"']+)["'][^>]*>/gi)) {
    if (!/\brel\s*=\s*["']?stylesheet\b/i.test(match[0])) continue;
    const href = match[1].split(/[?#]/, 1)[0];
    if (!href || /^[a-z][a-z0-9+.-]*:|^\/\//i.test(href)) continue;
    if (resolve(dirname(file.path), href) === entry) return true;
  }
  return false;
}

/// A parsed static import, dynamic import, or require whose specifier
/// resolves to the entry. A loading proof needs an actual import
/// statement: a quoted path sitting in a comment or an unrelated string
/// does not load the entry's CSS.
function importsEntry(file: { path: string; source: string }, entry: string): boolean {
  for (const match of maskJsComments(file.source).matchAll(IMPORT_SPECIFIERS)) {
    const spec = match[1] ?? match[2] ?? match[3];
    if (spec.startsWith(".") && resolve(dirname(file.path), spec) === entry) return true;
  }
  return false;
}
