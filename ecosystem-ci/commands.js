import { loadManifest, resolveFixture } from './run.js';
import { runExternalLifecycle, runLifecycle, runProductionSmoke } from './lifecycle.js';

export const commands = {
  async runEcosystemCase(context, id) {
    const browser = context.provider?.browser;
    if (!browser || typeof browser.newPage !== 'function') {
      throw new Error('ecosystem command requires the Vitest Playwright provider browser capability');
    }
    const manifest = await loadManifest();
    const project = manifest.projects.find((entry) => entry.id === id);
    if (!project) throw new Error(`unknown ecosystem case ${JSON.stringify(id)}`);
    const fixture = resolveFixture(manifest, project);
    const result = project.kind === 'smoke'
      ? await runProductionSmoke({ browser, project, fixture, artifactRoot: process.env.ECOSYSTEM_ARTIFACT_ROOT })
      : project.kind === 'external'
        ? await runExternalLifecycle({
            browser,
            project,
            packageFixture: manifest.projects.find(({ id: fixtureId }) => fixtureId === 'react-vite-css'),
            artifactRoot: process.env.ECOSYSTEM_ARTIFACT_ROOT,
          })
        : await runLifecycle({ browser, project, artifactRoot: process.env.ECOSYSTEM_ARTIFACT_ROOT });
    return { report: result.first ?? null, phases: result.ledger.phases.map(({ phase }) => phase) };
  },
};
