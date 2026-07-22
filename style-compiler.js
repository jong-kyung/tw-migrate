import { createRequire } from 'node:module';
import { basename, dirname, extname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { eachMapping, TraceMap } from '@jridgewell/trace-mapping';

const SASS_EXTENSIONS = new Set(['.scss', '.sass']);

export function isSassPath(path) {
  return SASS_EXTENSIONS.has(extname(path));
}

export function isPreprocessorPath(path) {
  return isSassPath(path) || extname(path) === '.less';
}

async function loadProjectModule(packageRoot, name, errorMessage) {
  const projectRequire = createRequire(join(packageRoot, 'package.json'));
  let modulePath;
  try {
    modulePath = projectRequire.resolve(name);
  } catch {
    throw new Error(errorMessage);
  }
  const imported = await import(pathToFileURL(modulePath));
  return imported.default ?? imported;
}

export function loadProjectSass(packageRoot) {
  return loadProjectModule(packageRoot, 'sass', 'Sass must be installed in the target project.');
}

export function loadProjectLess(packageRoot) {
  return loadProjectModule(packageRoot, 'less', 'Less must be installed in the target project.');
}

export async function compileSassEntry(sass, entryPath, source) {
  const options = {
    sourceMap: true,
    sourceMapIncludeSources: true,
  };
  const result = source === undefined
    ? await sass.compileAsync(entryPath, options)
    : await sass.compileStringAsync(source, {
      ...options,
      url: pathToFileURL(entryPath),
      syntax: extname(entryPath) === '.sass' ? 'indented' : 'scss',
    });

  if (!result.sourceMap) throw new Error(`Sass did not produce a source map for ${entryPath}`);
  return {
    css: result.css,
    loadedPaths: result.loadedUrls
      .filter((url) => url.protocol === 'file:')
      .map(fileURLToPath),
    sourceMappings: sourceMappings(result.sourceMap),
  };
}

export async function compileLessEntry(less, entryPath, source) {
  const result = await less.render(source, {
    filename: entryPath,
    sourceMap: {
      outputSourceFiles: true,
      sourceMapBasepath: dirname(entryPath),
      sourceMapOutputFilename: `${basename(entryPath, '.less')}.css`,
    },
  });
  return {
    css: result.css,
    loadedPaths: result.imports.map((path) => isAbsolute(path) ? path : resolve(dirname(entryPath), path)),
    sourceMappings: result.map ? sourceMappings(JSON.parse(result.map), dirname(entryPath)) : [],
  };
}

export function sourceMappings(sourceMap, sourceBase) {
  const mappings = [];
  eachMapping(new TraceMap(sourceMap), (mapping) => {
    if (mapping.source === null || mapping.originalLine === null || mapping.originalColumn === null) return;
    const sourcePath = sourcePathFromMap(mapping.source, sourceBase);
    if (!sourcePath) return;
    mappings.push({
      generatedLine: mapping.generatedLine - 1,
      generatedColumn: mapping.generatedColumn,
      sourcePath,
      originalLine: mapping.originalLine - 1,
      originalColumn: mapping.originalColumn,
    });
  });
  return mappings;
}

function sourcePathFromMap(source, sourceBase) {
  if (isAbsolute(source)) return resolve(source);
  try {
    const url = new URL(source);
    return url.protocol === 'file:' ? fileURLToPath(url) : undefined;
  } catch {
    return sourceBase ? resolve(sourceBase, source) : undefined;
  }
}
