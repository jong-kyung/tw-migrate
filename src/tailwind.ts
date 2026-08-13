import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, extname, isAbsolute, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { cssDirectives, stylesheetAnalysis } from "./native.ts";
import { loadProjectModule } from "./parser/style-compiler.ts";
import { isProjectInput, snapshotFile } from "./util/shared.ts";
import type { DesignSystem, LoadedTailwind, StylesheetLoader } from "./types.ts";

export function findTailwindEntries(
  stylePaths: string[],
  styleSources: Map<string, string>,
): string[] {
  return stylePaths.filter(
    (path) =>
      extname(path) === ".css" &&
      importSpecifiers(styleSources.get(path) ?? "").some(
        (specifier) => specifier === "tailwindcss" || specifier.startsWith("tailwindcss/"),
      ),
  );
}

export function selectTailwindEntry(entries: string[], configuredPath?: string): string {
  if (configuredPath) return configuredPath;
  if (entries.length === 0)
    throw new Error("No Tailwind v4 CSS entry was found. Pass --tailwind-css.");
  if (entries.length > 1)
    throw new Error("Multiple Tailwind CSS entries were found. Pass --tailwind-css.");
  return entries[0];
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

  const { __unstable__loadDesignSystem: loadDesignSystem } = await loadProjectModule<{
    __unstable__loadDesignSystem: (css: string, options: unknown) => Promise<DesignSystem>;
  }>(packageRoot, "tailwindcss", "Tailwind v4 must be installed in the target project.");
  const css = await snapshotFile(snapshots, tailwindCss);
  const base = dirname(tailwindCss);
  const tailwindRoot = dirname(packagePath);
  const loadModule = async (id: string, moduleBase: string) => {
    const path = createRequire(join(moduleBase, "package.json")).resolve(id);
    if (isProjectInput(workspaceRoot, path)) await snapshotFile(snapshots, path);
    const imported = await import(pathToFileURL(path).href);
    return { path, base: dirname(path), module: imported.default ?? imported };
  };
  const loadStylesheet: StylesheetLoader = async (id, sheetBase) => {
    let path;
    if (id === "tailwindcss") path = join(tailwindRoot, "index.css");
    else if (id.startsWith("tailwindcss/")) {
      const subpath = id.slice("tailwindcss/".length);
      path = join(tailwindRoot, subpath.endsWith(".css") ? subpath : `${subpath}.css`);
    } else if (id.startsWith(".") || isAbsolute(id)) path = resolve(sheetBase, id);
    else path = projectRequire.resolve(id);
    const content = isProjectInput(workspaceRoot, path)
      ? await snapshotFile(snapshots, path)
      : await readFile(path, "utf8");
    return { content, base: dirname(path) };
  };
  const defaultTheme = await readFile(join(dirname(packagePath), "theme.css"), "utf8");
  const graphSources: { path: string; source: string }[] = [];
  const themeTokens = {
    ...extractThemeTokens("tailwind-default.theme.css", defaultTheme),
    ...(await extractThemeTokensFromGraph(tailwindCss, css, base, loadStylesheet, graphSources)),
  };
  graphSources.push({ path: tailwindCss, source: css });
  const designSystem = await loadDesignSystem(css, { base, loadModule, loadStylesheet });
  const loadWith = (replacement: string): Promise<DesignSystem> =>
    loadDesignSystem(replacement, { base, loadModule, loadStylesheet });
  return { designSystem, css, path: tailwindCss, themeTokens, graphSources, loadWith };
}

/// Theme keys carry the loaded stylesheet's identity so the native cache
/// deduplicates repeated extraction instead of thrashing one shared slot.
function extractThemeTokens(key: string, css: string): Record<string, string> {
  return stylesheetAnalysis(key, css).themeTokens;
}

/// Import specifiers of one stylesheet through the structured directive
/// parser, covering quoted and url() forms at the top level only.
function importSpecifiers(css: string): string[] {
  return (cssDirectives(css) ?? []).flatMap((directive) =>
    directive !== null &&
    typeof directive === "object" &&
    "kind" in directive &&
    directive.kind === "import" &&
    "specifier" in directive &&
    typeof directive.specifier === "string"
      ? [directive.specifier]
      : [],
  );
}

async function extractThemeTokensFromGraph(
  identity: string,
  css: string,
  base: string,
  loadStylesheet: StylesheetLoader,
  graphSources: { path: string; source: string }[],
  seen = new Set<string>(),
): Promise<Record<string, string>> {
  const tokens: Record<string, string> = {};
  for (const spec of importSpecifiers(css)) {
    const key = `${base}\0${spec}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const loaded = await loadStylesheet(spec, base);
    // Tailwind's own stylesheets define no custom variants worth parsing.
    if (spec !== "tailwindcss" && !spec.startsWith("tailwindcss/")) {
      graphSources.push({ path: key, source: loaded.content });
    }
    Object.assign(
      tokens,
      await extractThemeTokensFromGraph(
        key,
        loaded.content,
        loaded.base,
        loadStylesheet,
        graphSources,
        seen,
      ),
    );
  }
  return Object.assign(tokens, extractThemeTokens(`${identity}.theme.css`, css));
}

export function invalidCandidates(system: DesignSystem, candidates: string[]): string[] {
  const generated = system.candidatesToCss(candidates);
  return candidates.filter((_, index) => generated[index] === null);
}
