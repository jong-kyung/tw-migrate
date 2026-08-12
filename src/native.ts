import { createRequire } from "node:module";

function errorCode(error: unknown): string | undefined {
  return error instanceof Error && "code" in error && typeof error.code === "string"
    ? error.code
    : undefined;
}

// Mirrors the generated binding.d.ts (napi build --dts), inlined so type
// checking does not depend on the addon having been built.
export interface StaticImportBinding {
  source: string;
  local: string;
}

export interface SourceImportRecord {
  specifier: string;
  typeOnly: boolean;
  dynamic: boolean;
}

export interface ExpressionAnalysis {
  staticString: string | null;
  vueModuleMember: string | null;
  usesCssModule: boolean;
}

export interface SourceAnalysis {
  imports: SourceImportRecord[];
  staticImports: string[];
  defaultImports: StaticImportBinding[];
  vueGlobPatterns: string[];
  hasDynamicImport: boolean;
  hasVueFallthroughMacro: boolean;
  usesCssModule: boolean;
}

export interface StylesheetAnalysis {
  references: string[];
  imports: { href: string; media: string; start: number; end: number }[];
  unverifiable: boolean;
  scopeEscapes: string[];
  scopeShadowCss: string[];
  scopeEscapesUnverifiable: boolean;
  themeTokens: Record<string, string>;
  globalAtRuleIdentities: string[];
}

interface Binding {
  collectMediaConditions: (request: string) => string;
  sourceAnalysis: (path: string, source: string) => string;
  stylesheetAnalysis: (path: string, source: string) => string;
  collectCssDirectives: (source: string) => string;
  mediaProbeKey: (css: string) => string;
  decodeSourceMap: (sourceMap: string) => string;
  planBatchMigration: (request: string) => string;
  expressionAnalysis: (path: string, source: string) => string;
  validateCss: (source: string) => void;
}

const require = createRequire(import.meta.url);
const targets: Record<string, string> = {
  "darwin-arm64": "darwin-arm64",
  "darwin-x64": "darwin-x64",
  "linux-arm64": "linux-arm64-gnu",
  "linux-x64": "linux-x64-gnu",
  "win32-x64": "win32-x64-msvc",
};
const target = targets[`${process.platform}-${process.arch}`];

if (!target) throw new Error(`Unsupported platform: ${process.platform}-${process.arch}`);

let binding: Binding;
try {
  // One level up is the repository root in development (src/) and the package
  // root in the installed layout (dist/); both hold the locally built addon.
  binding = require(`../tw-migrate.${target}.node`);
} catch (localError) {
  if (errorCode(localError) !== "MODULE_NOT_FOUND") throw localError;
  try {
    binding = require(`tw-migrate-${target}`);
  } catch (packageError) {
    if (errorCode(packageError) !== "MODULE_NOT_FOUND") throw packageError;
    throw new Error(
      `No tw-migrate native addon was found for ${target}. Reinstall the package or build it locally.`,
    );
  }
}

export const {
  collectMediaConditions,
  collectCssDirectives,
  mediaProbeKey,
  decodeSourceMap,
  planBatchMigration,
  validateCss,
} = binding;

const sourceAnalysisCache = new Map<string, { source: string; analysis: SourceAnalysis }>();
const stylesheetAnalysisCache = new Map<string, { source: string; analysis: StylesheetAnalysis }>();

export function expressionAnalysis(path: string, source: string): ExpressionAnalysis {
  return JSON.parse(binding.expressionAnalysis(path, source)) as ExpressionAnalysis;
}

export function sourceAnalysis(path: string, source: string): SourceAnalysis {
  const cached = sourceAnalysisCache.get(path);
  if (cached?.source === source) return cached.analysis;
  const analysis = JSON.parse(binding.sourceAnalysis(path, source)) as SourceAnalysis;
  sourceAnalysisCache.set(path, { source, analysis });
  return analysis;
}

export function stylesheetAnalysis(path: string, source: string): StylesheetAnalysis {
  const cached = stylesheetAnalysisCache.get(path);
  if (cached?.source === source) return cached.analysis;
  const analysis = JSON.parse(binding.stylesheetAnalysis(path, source)) as StylesheetAnalysis;
  stylesheetAnalysisCache.set(path, { source, analysis });
  return analysis;
}
