import { lstat, readFile } from "node:fs/promises";
import { basename, dirname, extname, join, relative, resolve, sep } from "node:path";

import { stylesheetAnalysis } from "../native.ts";
import { MISSING_STYLE_COMPILER_MESSAGES, isPreprocessorPath } from "../parser/style-compiler.ts";
import type { CssImport, MigrationFailure } from "../types.ts";

export const SOURCE_EXTENSIONS = new Set([
  ".html",
  ".vue",
  ".js",
  ".jsx",
  ".mjs",
  ".cjs",
  ".ts",
  ".tsx",
  ".mts",
  ".cts",
]);

export const STYLESHEET_SYNTAX = new Map([
  [".css", "css"],
  [".scss", "scss"],
  [".sass", "sass"],
  [".less", "less"],
]);

export const IGNORED_DIRECTORIES = new Set([".git", ".next", "build", "dist", "node_modules"]);

export const RECOVERABLE_INPUT_ERROR = "TW_MIGRATE_RECOVERABLE_INPUT:";

export function errorCode(error: unknown): string | undefined {
  return error instanceof Error && "code" in error && typeof error.code === "string"
    ? error.code
    : undefined;
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function isWithin(root: string, path: string): boolean {
  return path === root || path.startsWith(`${root}${sep}`);
}

export function isStylesheetPath(path: string): boolean {
  return STYLESHEET_SYNTAX.has(extname(path));
}

export function stylesheetSyntax(path: string): string | undefined {
  return STYLESHEET_SYNTAX.get(extname(path));
}

export function isStylesheetModule(path: string): boolean {
  const syntax = stylesheetSyntax(path);
  return syntax !== undefined && path.endsWith(`.module.${syntax}`);
}

export function isProjectInput(workspaceRoot: string, path: string): boolean {
  return (
    isWithin(workspaceRoot, path) &&
    !relative(workspaceRoot, path).split(/[\\/]/).includes("node_modules")
  );
}

export async function rejectSymlinkTarget(path: string, root: string): Promise<void> {
  for (let current = path; isWithin(root, current); current = dirname(current)) {
    if ((await lstat(current)).isSymbolicLink()) {
      throw new Error(`Refusing to migrate a symbolic-link target: ${path}`);
    }
    if (current === root) break;
  }
}

export async function snapshotFile(snapshots: Map<string, string>, path: string): Promise<string> {
  return recordSnapshot(snapshots, path, await readFile(path, "utf8"));
}

// The compiler just loaded this dependency, so its disappearance is a source
// change during planning -- a fatal integrity error `--force` must not
// downgrade to a recoverable package failure.
export async function snapshotLoadedSource(
  snapshots: Map<string, string>,
  path: string,
): Promise<string> {
  try {
    return await snapshotFile(snapshots, path);
  } catch (error) {
    if (errorCode(error) === "ENOENT") {
      throw new Error(`Source changed during planning: ${path}`);
    }
    throw error;
  }
}

export function recordSnapshot(
  snapshots: Map<string, string>,
  path: string,
  source: string,
): string {
  if (snapshots.has(path) && snapshots.get(path) !== source) {
    throw new Error(`Source changed during planning: ${path}`);
  }
  snapshots.set(path, source);
  return source;
}

export function packageFailure(
  workspaceRoot: string,
  packageRoot: string,
  error: unknown,
): MigrationFailure {
  const message = errorMessage(error);
  return {
    package: normalizedRelativePath(workspaceRoot, packageRoot) || ".",
    message: message.startsWith(RECOVERABLE_INPUT_ERROR)
      ? message.slice(RECOVERABLE_INPUT_ERROR.length)
      : message,
  };
}

export function isRecoverablePlanningError(error: unknown): boolean {
  return errorMessage(error).startsWith(RECOVERABLE_INPUT_ERROR);
}

export function isMissingStyleCompilerError(error: unknown): boolean {
  return MISSING_STYLE_COMPILER_MESSAGES.has(errorMessage(error));
}

export function isIntegrityError(error: unknown): boolean {
  return errorMessage(error).startsWith("Source changed during planning:");
}

export function normalizedRelativePath(root: string, path: string): string {
  return relative(root, path).split(sep).join("/");
}

export function indexStylesheetDependents(
  styleSources: Map<string, string>,
): Map<string, string[]> {
  const dependents = new Map<string, string[]>();
  const possibleTargets = [...styleSources.keys()].filter(
    (path) => isStylesheetModule(path) || isPreprocessorPath(path),
  );
  for (const [path, source] of styleSources) {
    let references: string[];
    try {
      const analysis = stylesheetAnalysis(path, source);
      references = analysis.references;
      if (analysis.unverifiable) {
        references = [...references, ...possibleTargets];
      }
    } catch {
      references = possibleTargets;
    }
    for (const reference of new Set(references)) {
      const targets = styleSources.has(reference)
        ? [reference]
        : stylesheetReferenceTargets(path, reference, styleSources);
      for (const target of targets) {
        if (target === path || (!isStylesheetModule(target) && !isPreprocessorPath(target)))
          continue;
        const paths = dependents.get(target) ?? [];
        paths.push(path);
        dependents.set(target, paths);
      }
    }
  }
  for (const [target, paths] of dependents) dependents.set(target, [...new Set(paths)].sort());
  return dependents;
}

export function addStyleDependent(
  styleDependents: Map<string, string[]>,
  target: string,
  importer: string,
): void {
  const paths = styleDependents.get(target) ?? [];
  if (paths.includes(importer)) return;
  paths.push(importer);
  paths.sort();
  styleDependents.set(target, paths);
}

export function stylesheetReferenceTargets(
  importer: string,
  reference: string,
  styleSources: Map<string, string>,
): string[] {
  const target = resolve(dirname(importer), reference);
  const candidates = STYLESHEET_SYNTAX.has(extname(target))
    ? [target]
    : [...STYLESHEET_SYNTAX.keys()].flatMap((syntax) => [
        `${target}${syntax}`,
        join(dirname(target), `_${basename(target)}${syntax}`),
        join(target, `_index${syntax}`),
        join(target, `index${syntax}`),
      ]);
  return candidates.filter((path) => styleSources.has(path));
}

export function cssImports(path: string, source: string): CssImport[] {
  return stylesheetAnalysis(path, source).imports;
}
