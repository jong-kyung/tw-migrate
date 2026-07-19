import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const platform = `${process.platform}-${process.arch}`;
const localNames = [
  `./tw-migrate.${platform}.node`,
  './tw-migrate.node',
];

let binding;
for (const name of localNames) {
  try {
    binding = require(name);
    break;
  } catch (error) {
    if (error.code !== 'MODULE_NOT_FOUND') throw error;
  }
}

if (!binding) {
  throw new Error(`No tw-migrate native addon was found for ${platform}. Reinstall the package or build it locally.`);
}

export const planMigration = binding.planMigration;
