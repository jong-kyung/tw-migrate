import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const targets = {
  'darwin-arm64': 'darwin-arm64',
  'darwin-x64': 'darwin-x64',
  'linux-arm64': 'linux-arm64-gnu',
  'linux-x64': 'linux-x64-gnu',
  'win32-x64': 'win32-x64-msvc',
};
const target = targets[`${process.platform}-${process.arch}`];

if (!target) throw new Error(`Unsupported platform: ${process.platform}-${process.arch}`);

let binding;
for (const load of [
  () => require(`./tw-migrate.${target}.node`),
  () => require(`tw-migrate-${target}`),
]) {
  try {
    binding = load();
    break;
  } catch (error) {
    if (error.code !== 'MODULE_NOT_FOUND') throw error;
  }
}

if (!binding) {
  throw new Error(`No tw-migrate native addon was found for ${target}. Reinstall the package or build it locally.`);
}

export const planMigration = binding.planMigration;
export const planBatchMigration = binding.planBatchMigration;
export const validateCss = binding.validateCss;
