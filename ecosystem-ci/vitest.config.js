import { playwright } from '@vitest/browser-playwright';
import { defineConfig } from 'vitest/config';

import { commands } from './commands.js';
import { externalLifecycleTimeoutMs, lifecycleTimeoutMs } from './lifecycle.js';
import { loadManifest, vitestProjects } from './run.js';

const manifest = await loadManifest();
const outerTimeoutMs = (project) =>
  (project.kind === 'external' ? externalLifecycleTimeoutMs : lifecycleTimeoutMs) + 60_000;

export default defineConfig({
  test: {
    projects: vitestProjects(manifest.projects).map((project) => ({
      test: {
        name: project.id,
        include: ['ecosystem-ci/tests/ecosystem.browser.js'],
        testTimeout: outerTimeoutMs(project),
        hookTimeout: outerTimeoutMs(project),
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
