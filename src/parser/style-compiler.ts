import { realpath } from "node:fs/promises";
import { createRequire } from "node:module";
import { basename, dirname, extname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { decodeSourceMap } from "../native.ts";

const SASS_EXTENSIONS = new Set([".scss", ".sass"]);

// Minimal structural types for the compilers loaded from the target project;
// sass and less are not dependencies of tw-migrate itself.
interface RawSourceMap {
  sourceRoot?: string;
  [key: string]: unknown;
}

interface SassCompileOptions {
  sourceMap: boolean;
  sourceMapIncludeSources: boolean;
  url?: URL;
  syntax?: "indented" | "scss";
}

interface SassCompileResult {
  css: string;
  sourceMap?: RawSourceMap;
  loadedUrls: URL[];
}

export interface SassCompiler {
  compileAsync: (path: string, options: SassCompileOptions) => Promise<SassCompileResult>;
  compileStringAsync: (source: string, options: SassCompileOptions) => Promise<SassCompileResult>;
}

interface LessSourceMapOptions {
  outputSourceFiles: boolean;
  sourceMapBasepath: string;
  sourceMapOutputFilename: string;
}

interface LessRenderResult {
  css: string;
  imports: string[];
  map?: string;
}

export interface LessCompiler {
  render: (
    source: string,
    options: { filename: string; sourceMap: LessSourceMapOptions },
  ) => Promise<LessRenderResult>;
}

// The decoded shape produced by the native decodeSourceMap export.
interface DecodedSourceMapping {
  generatedLine: number;
  generatedColumn: number;
  source: string;
  originalLine: number;
  originalColumn: number;
}

export interface SourceMapping extends Omit<DecodedSourceMapping, "source"> {
  sourcePath: string;
}

export interface CompiledStyleEntry {
  css: string;
  loadedPaths: string[];
  sourceMappings: SourceMapping[];
}

export function isSassPath(path: string): boolean {
  return SASS_EXTENSIONS.has(extname(path));
}

export function isPreprocessorPath(path: string): boolean {
  return isSassPath(path) || extname(path) === ".less";
}

async function loadProjectModule<Compiler>(
  packageRoot: string,
  name: string,
  errorMessage: string,
): Promise<Compiler> {
  const projectRequire = createRequire(join(packageRoot, "package.json"));
  let modulePath;
  try {
    modulePath = projectRequire.resolve(name);
  } catch {
    throw new Error(errorMessage);
  }
  const imported = await import(pathToFileURL(modulePath).href);
  return imported.default ?? imported;
}

export function loadProjectSass(packageRoot: string): Promise<SassCompiler> {
  return loadProjectModule(packageRoot, "sass", "Sass must be installed in the target project.");
}

export function loadProjectLess(packageRoot: string): Promise<LessCompiler> {
  return loadProjectModule(packageRoot, "less", "Less must be installed in the target project.");
}

export async function compileSassEntry(
  sass: SassCompiler,
  entryPath: string,
  source?: string,
  { virtualEntry = false }: { virtualEntry?: boolean } = {},
): Promise<CompiledStyleEntry> {
  const options: SassCompileOptions = {
    sourceMap: true,
    sourceMapIncludeSources: true,
  };
  const result =
    source === undefined
      ? await sass.compileAsync(entryPath, options)
      : await sass.compileStringAsync(source, {
          ...options,
          url: pathToFileURL(entryPath),
          syntax: extname(entryPath) === ".sass" ? "indented" : "scss",
        });

  if (!result.sourceMap) throw new Error(`Sass did not produce a source map for ${entryPath}`);
  const loadedPaths = result.loadedUrls
    .filter((url) => url.protocol === "file:")
    .map((url) => fileURLToPath(url));
  const mappings = sourceMappings(result.sourceMap);
  const normalizedPaths = await normalizeEntryPaths(
    [...loadedPaths, ...mappings.map((mapping) => mapping.sourcePath)],
    entryPath,
    virtualEntry,
  );
  return {
    css: result.css,
    loadedPaths: loadedPaths.map((path) => normalizedPaths.get(path) ?? path),
    sourceMappings: mappings.map((mapping) => ({
      ...mapping,
      sourcePath: normalizedPaths.get(mapping.sourcePath) ?? mapping.sourcePath,
    })),
  };
}

async function normalizeEntryPaths(
  paths: string[],
  entryPath: string,
  virtualEntry: boolean,
): Promise<Map<string, string>> {
  // A virtual SFC block entry never exists on disk; a real entry that
  // vanished mid-run must keep failing as a source-integrity error.
  const canonicalEntryPath = virtualEntry ? entryPath : await realpath(entryPath);
  return new Map(
    await Promise.all(
      [...new Set(paths)].map(async (path): Promise<[string, string]> => {
        if (path === entryPath) return [path, path];
        try {
          return [path, (await realpath(path)) === canonicalEntryPath ? entryPath : path];
        } catch {
          return [path, path];
        }
      }),
    ),
  );
}

export async function compileLessEntry(
  less: LessCompiler,
  entryPath: string,
  source: string,
): Promise<CompiledStyleEntry> {
  const result = await less.render(source, {
    filename: entryPath,
    sourceMap: {
      outputSourceFiles: true,
      sourceMapBasepath: dirname(entryPath),
      sourceMapOutputFilename: `${basename(entryPath, ".less")}.css`,
    },
  });
  return {
    css: result.css,
    loadedPaths: result.imports.map((path) =>
      isAbsolute(path) ? path : resolve(dirname(entryPath), path),
    ),
    sourceMappings: result.map ? sourceMappings(JSON.parse(result.map), dirname(entryPath)) : [],
  };
}

export function sourceMappings(sourceMap: RawSourceMap, sourceBase?: string): SourceMapping[] {
  const decoded: DecodedSourceMapping[] = JSON.parse(decodeSourceMap(JSON.stringify(sourceMap)));
  return decoded.flatMap(({ source, ...mapping }) => {
    const sourcePath = sourcePathFromMap(
      sourceReferenceFromMap(source, sourceMap.sourceRoot),
      sourceBase,
    );
    return sourcePath ? [{ ...mapping, sourcePath }] : [];
  });
}

function sourceReferenceFromMap(source: string, sourceRoot?: string): string {
  if (!sourceRoot) return source;
  if (isAbsolute(sourceRoot)) return resolve(sourceRoot, source);
  try {
    const root = new URL(sourceRoot);
    root.pathname = `${root.pathname.replace(/\/+$/, "")}/`;
    return new URL(source, root).href;
  } catch {
    return join(sourceRoot, source);
  }
}

function sourcePathFromMap(source: string, sourceBase?: string): string | undefined {
  if (isAbsolute(source)) return resolve(source);
  try {
    const url = new URL(source);
    return url.protocol === "file:" ? resolve(fileURLToPath(url)) : undefined;
  } catch {
    return sourceBase ? resolve(sourceBase, source) : undefined;
  }
}
