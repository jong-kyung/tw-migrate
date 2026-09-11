import { commands } from "vite-plus/test/browser";
import { expect, inject, test } from "vite-plus/test";

import type { MigrationReport, Project } from "../types.ts";

declare module "vitest" {
  interface ProvidedContext {
    ecosystemProject: Project;
  }
}

declare module "vitest/browser" {
  interface BrowserCommands {
    assertProcessTreeTeardown: () => Promise<void>;
    runEcosystemCase: (id: string) => Promise<{ report: MigrationReport | null; phases: string[] }>;
  }
}

test.skipIf(inject("ecosystemProject").id !== "react-vite-css")(
  "stops descendants after parent exit and command timeout on POSIX",
  async () => {
    await commands.assertProcessTreeTeardown();
  },
);

test("preserves the fixture through the installed migration lifecycle", async () => {
  const project = inject("ecosystemProject");
  const result = await commands.runEcosystemCase(project.id);
  expect(result.phases.at(-1)).toBe("complete");
  if (project.kind !== "smoke") expect(result.report!.candidates).toContain(project.source.after);
  if (project.kind === "controlled") {
    if (project.id === "vue-vite-css") {
      expect(result.report!.candidates).toContain("p-[17px]");
      expect(result.report!.changedFiles).not.toContain("src/Child.vue");
    }
    expect(result.phases.includes("idempotency")).toBe(project.idempotency === true);
  } else if (project.kind === "smoke") {
    expect(result.phases).not.toContain("second-cli-started");
  }
});
