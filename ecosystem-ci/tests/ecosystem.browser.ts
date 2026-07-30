import { commands } from "vite-plus/test/browser";
import { expect, inject, test } from "vite-plus/test";

import type { MigrationReport, Project } from "../types.ts";

type EcosystemBrowserCommands = typeof commands & {
  runEcosystemCase: (id: string) => Promise<{ report: MigrationReport | null; phases: string[] }>;
};

test("preserves the fixture through the installed migration lifecycle", async () => {
  const project = (inject as (key: "ecosystemProject") => Project)("ecosystemProject");
  const result = await (commands as EcosystemBrowserCommands).runEcosystemCase(project.id);
  expect(result.phases.at(-1)).toBe("complete");
  if (project.kind !== "smoke") expect(result.report!.candidates).toContain(project.source.after);
});
