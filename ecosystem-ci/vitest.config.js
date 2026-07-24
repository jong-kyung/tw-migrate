import { playwright } from '@vitest/browser-playwright';
import { defineConfig } from 'vitest/config';

import { commands } from './commands.js';
import { lifecycleTimeoutMs } from './lifecycle.js';
import { loadManifest } from './run.js';

const manifest = await loadManifest();
const outerTimeoutMs = lifecycleTimeoutMs + 60_000;

export default defineConfig({
  test: {
    projects: manifest.projects.filter(({ kind }) => kind !== 'external').map((project) => ({
      test: {
        name: project.id,
        include: ['ecosystem-ci/tests/ecosystem.browser.js'],
        testTimeout: outerTimeoutMs,
        hookTimeout: outerTimeoutMs,
        provide: { ecosystemProject: project },
        browser: {
          enabled: true,
          headless: true,
          provider: playwright(),
          instances: [{ browser: 'chromium' }],
          commands,
        },
      },
    })),
  },
});
