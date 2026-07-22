import { createRequire } from 'node:module';
import { extname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { decodedMappings, originalPositionFor, TraceMap } from '@jridgewell/trace-mapping';

const SASS_EXTENSIONS = new Set(['.scss', '.sass']);

export function isSassPath(path) {
  return SASS_EXTENSIONS.has(extname(path));
}

export async function loadProjectSass(packageRoot) {
  const projectRequire = createRequire(join(packageRoot, 'package.json'));
  let modulePath;
  try {
    modulePath = projectRequire.resolve('sass');
  } catch {
    throw new Error('Sass must be installed in the target project.');
  }
  const imported = await import(pathToFileURL(modulePath));
  return imported.default ?? imported;
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

function sourceMappings(sourceMap) {
  const traceMap = new TraceMap(sourceMap);
  const mappings = [];
  for (const [generatedLine, segments] of decodedMappings(traceMap).entries()) {
    for (const segment of segments) {
      if (segment.length < 4) continue;
      const generatedColumn = segment[0];
      const original = originalPositionFor(traceMap, {
        line: generatedLine + 1,
        column: generatedColumn,
      });
      if (original.source === null || original.line === null || original.column === null) continue;
      let sourcePath;
      try {
        const url = new URL(original.source);
        if (url.protocol !== 'file:') continue;
        sourcePath = fileURLToPath(url);
      } catch {
        continue;
      }
      mappings.push({
        generatedLine,
        generatedColumn,
        sourcePath,
        originalLine: original.line - 1,
        originalColumn: original.column,
      });
    }
  }
  return mappings;
}
