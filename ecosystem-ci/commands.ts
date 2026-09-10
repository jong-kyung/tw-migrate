import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { setTimeout as delay } from "node:timers/promises";

import type { Browser } from "playwright";
import type { BrowserCommandContext } from "vite-plus/test/node";

import { loadManifest, resolveFixture } from "./run.ts";
import { runExternalLifecycle, runLifecycle, runProductionSmoke } from "./lifecycle.ts";
import type { ControlledProject } from "./types.ts";
import { terminateTree, waitForChild } from "./shared.ts";

export const commands = {
  async assertProcessTreeTeardown() {
    if (process.platform === "win32") return;
    const assertDescendantStopped = async (url: string) => {
      const deadline = Date.now() + 3_000;
      while (Date.now() < deadline) {
        try {
          await (await fetch(url, { signal: AbortSignal.timeout(500) })).text();
        } catch (error) {
          // A timeout is not proof of teardown: require a closed listener.
          const code = ((error as Error).cause as NodeJS.ErrnoException)?.code;
          if (code === "ECONNREFUSED") return;
          if (code !== "ECONNRESET") throw error;
        }
        await delay(50);
      }
      assert.fail("descendant server must stop after teardown");
    };
    // The descendant ignores TERM and serves only loopback. Both processes
    // self-expire as a backstop if the test runner itself is interrupted.
    const descendantSource = `
      process.on('SIGTERM', () => {});
      setTimeout(() => process.exit(0), 20000);
      require('node:http').createServer((req, res) => res.end('descendant'))
        .listen(0, '127.0.0.1', function () {
          process.send(this.address().port);
        });
    `;
    for (const mode of ["already-exited", "exits-on-TERM", "timeout"]) {
      const parent = spawn(
        process.execPath,
        [
          "-e",
          `
            const child = require('node:child_process').spawn(
              process.execPath, ['-e', ${JSON.stringify(descendantSource)}],
              {stdio: ['ignore', 'ignore', 'ignore', 'ipc']}
            );
            setTimeout(() => process.exit(0), 20000);
            process.on('SIGTERM', () => process.exit(0));
            child.once('message', port => {
              process.send(port);
              if (${JSON.stringify(mode)} === 'already-exited') process.exit(0);
            });
          `,
        ],
        { detached: true, stdio: ["ignore", "ignore", "ignore", "ipc"] },
      );
      const exited = once(parent, "exit", { signal: AbortSignal.timeout(10_000) });
      let url: string | undefined;
      try {
        const [port] = await once(parent, "message", { signal: AbortSignal.timeout(5_000) });
        if (mode === "already-exited") await exited;
        assert.equal(parent.exitCode, mode === "already-exited" ? 0 : null);
        url = `http://127.0.0.1:${port}`;
        assert.equal(
          await (await fetch(url, { signal: AbortSignal.timeout(1_000) })).text(),
          "descendant",
        );
        if (mode === "timeout") {
          await assert.rejects(
            waitForChild(parent, { timeoutMs: 100 }),
            /command timed out after 100ms/,
          );
        } else {
          await terminateTree(parent);
        }
        await exited;
        await assertDescendantStopped(url);
        await terminateTree(parent);
      } finally {
        // Never target anything except this test's detached process group.
        try {
          process.kill(-parent.pid!, "SIGKILL");
        } catch (error) {
          assert.equal((error as NodeJS.ErrnoException).code, "ESRCH");
        }
        await exited;
        if (url) await assertDescendantStopped(url);
      }
    }
  },
  async runEcosystemCase(context: BrowserCommandContext, id: string) {
    // Only the Playwright provider exposes a Browser; the base type does not.
    const browser = (context.provider as { browser?: Browser } | undefined)?.browser;
    if (!browser || typeof browser.newPage !== "function") {
      throw new Error(
        "ecosystem command requires the Vitest Playwright provider browser capability",
      );
    }
    const manifest = await loadManifest();
    const project = manifest.projects.find((entry) => entry.id === id);
    if (!project) throw new Error(`unknown ecosystem case ${JSON.stringify(id)}`);
    const fixture = resolveFixture(manifest, project);
    const result =
      project.kind === "smoke"
        ? // validateManifest pins every smoke fixture to a controlled case.
          await runProductionSmoke({
            browser,
            project,
            fixture: fixture as ControlledProject,
            artifactRoot: process.env.ECOSYSTEM_ARTIFACT_ROOT,
          })
        : project.kind === "external"
          ? await runExternalLifecycle({
              browser,
              project,
              // validateManifest pins this controlled case as the package driver.
              packageFixture: manifest.projects.find(
                ({ id: fixtureId }) => fixtureId === "react-vite-css",
              ) as ControlledProject,
              artifactRoot: process.env.ECOSYSTEM_ARTIFACT_ROOT,
            })
          : await runLifecycle({
              browser,
              project,
              artifactRoot: process.env.ECOSYSTEM_ARTIFACT_ROOT,
            });
    return { report: result.first ?? null, phases: result.ledger.phases.map(({ phase }) => phase) };
  },
};
