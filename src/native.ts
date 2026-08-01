import { createRequire } from "node:module";

// Mirrors the generated binding.d.ts (napi build --dts), inlined so type
// checking does not depend on the addon having been built.
export interface StaticImportBinding {
  source: string;
  local: string;
}

interface Binding {
  decodeSourceMap: (sourceMap: string) => string;
  planBatchMigration: (request: string) => string;
  staticImportBindings: (path: string, source: string) => StaticImportBinding[];
  staticImports: (path: string, source: string) => string[];
  staticStringExpression: (path: string, source: string) => string | null;
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

let binding: Binding | undefined;
for (const load of [
  // One level up is the repository root in development (src/) and the package
  // root in the installed layout (dist/); both hold the locally built addon.
  () => require(`../tw-migrate.${target}.node`) as Binding,
  () => require(`tw-migrate-${target}`) as Binding,
]) {
  try {
    binding = load();
    break;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "MODULE_NOT_FOUND") throw error;
  }
}

if (!binding) {
  throw new Error(
    `No tw-migrate native addon was found for ${target}. Reinstall the package or build it locally.`,
  );
}

export const {
  decodeSourceMap,
  planBatchMigration,
  staticImportBindings,
  staticImports,
  staticStringExpression,
  validateCss,
} = binding;
