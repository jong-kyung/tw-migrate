import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { extension, isProjectInput, maskCssComments, snapshotFile } from "./util/shared.ts";
import type { LoadedTailwind, StylesheetLoader } from "./types.ts";

export function resolveTailwindEntry(
  stylePaths: string[],
  styleSources: Map<string, string>,
  configuredPath?: string,
): { path: string; entries: string[] } {
  const entries = stylePaths.filter((path) => {
    if (extension(path) !== ".css") return false;
    const source = maskCssComments(styleSources.get(path) ?? "");
    return /@import\s+["']tailwindcss(?:\/[^"']*)?["']/.test(source);
  });
  if (configuredPath) return { path: configuredPath, entries };
  if (entries.length === 0)
    throw new Error("No Tailwind v4 CSS entry was found. Pass --tailwind-css.");
  if (entries.length > 1)
    throw new Error("Multiple Tailwind CSS entries were found. Pass --tailwind-css.");
  return { path: entries[0], entries };
}

export async function loadTailwind(
  packageRoot: string,
  tailwindCss: string,
  snapshots: Map<string, string>,
  workspaceRoot: string,
): Promise<LoadedTailwind> {
  const projectRequire = createRequire(join(packageRoot, "package.json"));
  let packagePath;
  try {
    packagePath = projectRequire.resolve("tailwindcss/package.json");
  } catch {
    throw new Error("Tailwind v4 must be installed in the target project.");
  }
  const packageJson = JSON.parse(await readFile(packagePath, "utf8"));
  if (!String(packageJson.version).startsWith("4."))
    throw new Error(`Tailwind v4 is required; found ${packageJson.version}.`);

  const modulePath = projectRequire.resolve("tailwindcss");
  const tailwindModule = await import(pathToFileURL(modulePath).href);
  const { __unstable__loadDesignSystem: loadDesignSystem } =
    tailwindModule.default ?? tailwindModule;
  const css = await snapshotFile(snapshots, tailwindCss);
  const base = dirname(tailwindCss);
  const loadModule = createModuleLoader(snapshots, workspaceRoot);
  const loadStylesheet = createStylesheetLoader(
    projectRequire,
    packagePath,
    snapshots,
    workspaceRoot,
  );
  const defaultTheme = await readFile(join(dirname(packagePath), "theme.css"), "utf8");
  const themeTokens = {
    ...extractThemeTokens(defaultTheme),
    ...(await extractThemeTokensFromGraph(css, base, loadStylesheet)),
  };
  const designSystem = await loadDesignSystem(css, { base, loadModule, loadStylesheet });
  return { designSystem, css, path: tailwindCss, themeTokens };
}

function extractThemeTokens(css: string): Record<string, string> {
  const tokens: Record<string, string> = {};
  for (const block of css.matchAll(/@theme[^{]*\{([^}]*)\}/gs)) {
    for (const match of block[1].matchAll(/--([\w-]+):\s*([^;{}]+);/g))
      tokens[match[1]] = match[2].trim();
  }
  return tokens;
}

async function extractThemeTokensFromGraph(
  css: string,
  base: string,
  loadStylesheet: StylesheetLoader,
  seen = new Set<string>(),
): Promise<Record<string, string>> {
  const tokens: Record<string, string> = {};
  for (const match of css.matchAll(/@import\s+["']([^"']+)["']/g)) {
    const key = `${base}\0${match[1]}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const loaded = await loadStylesheet(match[1], base);
    Object.assign(
      tokens,
      await extractThemeTokensFromGraph(loaded.content, loaded.base, loadStylesheet, seen),
    );
  }
  return Object.assign(tokens, extractThemeTokens(css));
}

export function invalidCandidates(tailwind: LoadedTailwind, candidates: string[]): string[] {
  const generated = tailwind.designSystem.candidatesToCss(candidates);
  return candidates.filter((_, index) => generated[index] === null);
}

function createModuleLoader(snapshots: Map<string, string>, workspaceRoot: string) {
  return async (id: string, base: string) => {
    const path = createRequire(join(base, "package.json")).resolve(id);
    if (isProjectInput(workspaceRoot, path)) await snapshotFile(snapshots, path);
    const imported = await import(pathToFileURL(path).href);
    return { path, base: dirname(path), module: imported.default ?? imported };
  };
}

function createStylesheetLoader(
  projectRequire: NodeJS.Require,
  tailwindPackagePath: string,
  snapshots: Map<string, string>,
  workspaceRoot: string,
): StylesheetLoader {
  const tailwindRoot = dirname(tailwindPackagePath);
  return async (id, base) => {
    let path;
    if (id === "tailwindcss") path = join(tailwindRoot, "index.css");
    else if (id.startsWith("tailwindcss/")) {
      const subpath = id.slice("tailwindcss/".length);
      path = join(tailwindRoot, subpath.endsWith(".css") ? subpath : `${subpath}.css`);
    } else if (id.startsWith(".") || isAbsolute(id)) path = resolve(base, id);
    else path = projectRequire.resolve(id);
    const content = isProjectInput(workspaceRoot, path)
      ? await snapshotFile(snapshots, path)
      : await readFile(path, "utf8");
    return { content, base: dirname(path) };
  };
}
