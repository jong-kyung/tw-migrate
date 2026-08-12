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
  vueGlobUnverifiable: boolean;
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
  selectorsUnverifiable: boolean;
  themeTokens: Record<string, string>;
  globalAtRuleIdentities: string[];
  globalAtRulesUnverifiable: boolean;
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

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

function decode<T>(label: string, json: string, valid: (value: unknown) => value is T): T {
  const value: unknown = JSON.parse(json);
  if (!valid(value)) throw new Error(`Invalid native ${label} result`);
  return value;
}

function sourceImport(value: unknown): value is SourceImportRecord {
  return (
    object(value) &&
    typeof value.specifier === "string" &&
    typeof value.typeOnly === "boolean" &&
    typeof value.dynamic === "boolean"
  );
}

function sourceAnalysisResult(value: unknown): value is SourceAnalysis {
  return (
    object(value) &&
    Array.isArray(value.imports) &&
    value.imports.every(sourceImport) &&
    Array.isArray(value.staticImports) &&
    value.staticImports.every((item) => typeof item === "string") &&
    Array.isArray(value.defaultImports) &&
    value.defaultImports.every(
      (item) => object(item) && typeof item.source === "string" && typeof item.local === "string",
    ) &&
    Array.isArray(value.vueGlobPatterns) &&
    value.vueGlobPatterns.every((item) => typeof item === "string") &&
    [
      value.vueGlobUnverifiable,
      value.hasDynamicImport,
      value.hasVueFallthroughMacro,
      value.usesCssModule,
    ].every((item) => typeof item === "boolean")
  );
}

const sourceAnalysisCache = new Map<string, { source: string; analysis: SourceAnalysis }>();
const stylesheetAnalysisCache = new Map<string, { source: string; analysis: StylesheetAnalysis }>();

export function expressionAnalysis(path: string, source: string): ExpressionAnalysis {
  return decode(
    "expression analysis",
    binding.expressionAnalysis(path, source),
    (value): value is ExpressionAnalysis =>
      object(value) &&
      (typeof value.staticString === "string" || value.staticString === null) &&
      (typeof value.vueModuleMember === "string" || value.vueModuleMember === null) &&
      typeof value.usesCssModule === "boolean",
  );
}

export function sourceAnalysis(path: string, source: string): SourceAnalysis {
  const cached = sourceAnalysisCache.get(path);
  if (cached?.source === source) return cached.analysis;
  const analysis = decode(
    "source analysis",
    binding.sourceAnalysis(path, source),
    sourceAnalysisResult,
  );
  sourceAnalysisCache.set(path, { source, analysis });
  return analysis;
}

export function stylesheetAnalysis(path: string, source: string): StylesheetAnalysis {
  const cached = stylesheetAnalysisCache.get(path);
  if (cached?.source === source) return cached.analysis;
  const analysis = decode(
    "stylesheet analysis",
    binding.stylesheetAnalysis(path, source),
    (value): value is StylesheetAnalysis =>
      object(value) &&
      Array.isArray(value.references) &&
      value.references.every((item) => typeof item === "string") &&
      Array.isArray(value.imports) &&
      value.imports.every(
        (item) =>
          object(item) &&
          typeof item.href === "string" &&
          typeof item.media === "string" &&
          typeof item.start === "number" &&
          typeof item.end === "number",
      ) &&
      [value.unverifiable, value.scopeEscapesUnverifiable, value.selectorsUnverifiable].every(
        (item) => typeof item === "boolean",
      ) &&
      Array.isArray(value.scopeEscapes) &&
      value.scopeEscapes.every((item) => typeof item === "string") &&
      Array.isArray(value.scopeShadowCss) &&
      value.scopeShadowCss.every((item) => typeof item === "string") &&
      object(value.themeTokens) &&
      Object.values(value.themeTokens).every((item) => typeof item === "string") &&
      Array.isArray(value.globalAtRuleIdentities) &&
      value.globalAtRuleIdentities.every((item) => typeof item === "string") &&
      typeof value.globalAtRulesUnverifiable === "boolean",
  );
  stylesheetAnalysisCache.set(path, { source, analysis });
  return analysis;
}
