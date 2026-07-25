import type { Browser } from 'playwright';
import type { BrowserCommandContext } from 'vitest/node';

import { loadManifest, resolveFixture } from './run.ts';
import { runExternalLifecycle, runLifecycle, runProductionSmoke } from './lifecycle.ts';
import type { ControlledProject } from './types.ts';

export const commands = {
  async runEcosystemCase(context: BrowserCommandContext, id: string) {
    // Only the Playwright provider exposes a Browser; the base type does not.
    const browser = (context.provider as { browser?: Browser } | undefined)?.browser;
    if (!browser || typeof browser.newPage !== 'function') {
      throw new Error('ecosystem command requires the Vitest Playwright provider browser capability');
    }
    const manifest = await loadManifest();
    const project = manifest.projects.find((entry) => entry.id === id);
    if (!project) throw new Error(`unknown ecosystem case ${JSON.stringify(id)}`);
    const fixture = resolveFixture(manifest, project);
    const result = project.kind === 'smoke'
      // validateManifest pins every smoke fixture to a controlled case.
      ? await runProductionSmoke({ browser, project, fixture: fixture as ControlledProject, artifactRoot: process.env.ECOSYSTEM_ARTIFACT_ROOT })
      : project.kind === 'external'
        ? await runExternalLifecycle({
            browser,
            project,
            // validateManifest pins this controlled case as the package driver.
            packageFixture: manifest.projects
              .find(({ id: fixtureId }) => fixtureId === 'react-vite-css') as ControlledProject,
            artifactRoot: process.env.ECOSYSTEM_ARTIFACT_ROOT,
          })
        : await runLifecycle({ browser, project, artifactRoot: process.env.ECOSYSTEM_ARTIFACT_ROOT });
    return { report: result.first ?? null, phases: result.ledger.phases.map(({ phase }) => phase) };
  },
};
