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

import { isWithin, maskCssComments, normalizedRelativePath } from "../util/shared.ts";
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

/// How the entry's utility detection covers the child package, or null when
/// coverage cannot be proven. A literal `@source` scope is scanned
/// regardless of ignore rules; automatic detection additionally requires
/// each consumer path to pass the effective scanner ignore rules.
export function scanProof(options: {
  entry: string;
  entrySource: string;
  packageRoot: string;
}): "literal" | "automatic" | null {
  const source = maskCssComments(options.entrySource);
  const base = dirname(options.entry);
  const containsChild = (scope: string): boolean => {
    const resolved = resolve(base, scope.replace(/[/\\]\*.*$/, ""));
    return isWithin(resolved, options.packageRoot) || isWithin(options.packageRoot, resolved);
  };
  // An exclusion covering the child defeats both proofs conservatively.
  for (const match of source.matchAll(/@source\s+not\s+["']([^"']+)["']/g)) {
    if (containsChild(match[1])) return null;
  }
  for (const match of source.matchAll(/@source\s+["']([^"']+)["']/g)) {
    if (containsChild(match[1])) return "literal";
  }
  const importSource = source.match(/@import\s+["']tailwindcss["'][^;]*\bsource\(([^)]*)\)/);
  if (importSource) {
    const argument = importSource[1].trim();
    if (argument === "none") return null;
    const literal = argument.match(/^["']([^"']+)["']$/);
    return literal && containsChild(literal[1]) ? "literal" : null;
  }
  return isWithin(base, options.packageRoot) ? "automatic" : null;
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
    for (const match of file.source.matchAll(IMPORT_SPECIFIERS)) {
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
    if (resolved !== undefined) entryFiles.push(resolved);
  }
  return reachableFrom(entryFiles, importEdges(files));
}

export interface SharedEntryProofOptions {
  packageRoot: string;
  entry: string;
  entrySource: string;
  /// The child's prepared sources, including HTML stylesheet contexts.
  packageSources: PreparedSourceFile[];
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

  const loaders = options.packageSources
    .filter((file) => options.writable(file.path))
    .filter(
      (file) =>
        (file.htmlStylesheets ?? []).some((context) => context.cssPath === options.entry) ||
        (extname(file.path) !== ".html" && fileImportsStylesheet(file, options.entry)),
    )
    .map((file) => file.path);
  if (loaders.length === 0) return null;

  const edges = importEdges(options.packageSources);
  const reachable = reachableFrom(loaders, edges);
  const exposed = exposedFiles(options.packageRoot, options.packageJson, options.packageSources);

  return {
    provenStyle: (stylePath, consumers) =>
      consumers.every(
        (consumer) =>
          reachable.has(consumer.path) &&
          (exposed === "all" ? false : !exposed.has(consumer.path)) &&
          (scan === "literal" || !options.ignoredPaths.has(consumer.path)),
      ),
  };
}

/// A quoted relative import of the stylesheet, matching the reference shape
/// used for consumer detection elsewhere.
function fileImportsStylesheet(file: { path: string; source: string }, stylePath: string): boolean {
  let importPath = normalizedRelativePath(dirname(file.path), stylePath);
  if (!importPath.startsWith(".")) importPath = `./${importPath}`;
  return [`'${importPath}'`, `"${importPath}"`, `\`${importPath}\``].some((literal) =>
    file.source.includes(literal),
  );
}
