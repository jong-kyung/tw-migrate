import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, onTestFinished, test } from "vite-plus/test";

import { migrate } from "../src/index.ts";
import { usedGeneratedDefinitions } from "../src/plan/media.ts";
import type { Plan, PlanRule } from "../src/types.ts";
const mediaCss =
  ".button { padding: 13px; }\n@media screen and (max-width: 700px) { .button { margin: 7px; } }\n";
const consumerTsx =
  "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n";
const screenDefinition = "@custom-variant screen {\n  @media screen {\n    @slot;\n  }\n}\n";
const widthDefinition =
  "@custom-variant width-lte-700px {\n  @media (width <= 700px) {\n    @slot;\n  }\n}\n";

async function tempDir(): Promise<string> {
  await mkdir(".tmp", { recursive: true });
  const cwd = await mkdtemp(join(process.cwd(), ".tmp", "fixture-"));
  onTestFinished(() => rm(cwd, { recursive: true, force: true }));
  return cwd;
}

async function fixture({
  css = mediaCss,
  tsx = consumerTsx,
  entry = '@import "tailwindcss";\n',
} = {}) {
  const cwd = await tempDir();
  await Promise.all([
    writeFile(join(cwd, "package.json"), '{"private":true}'),
    writeFile(join(cwd, "globals.css"), entry),
    writeFile(join(cwd, "Button.module.css"), css),
    writeFile(join(cwd, "Button.tsx"), tsx),
  ]);
  return cwd;
}

test("extracts media components into entry definitions and stacked variants", async () => {
  const cwd = await fixture();
  const report = await migrate({ cwd, extractMediaQueries: true, write: true });

  expect(report.candidates).toEqual(["p-[13px]", "screen:width-lte-700px:m-[7px]"]);
  expect(report.changedFiles).toEqual(["Button.module.css", "Button.tsx", "globals.css"]);
  expect(await readFile(join(cwd, "globals.css"), "utf8")).toBe(
    `@import "tailwindcss";\n\n${screenDefinition}\n${widthDefinition}`,
  );
  expect(await readFile(join(cwd, "Button.tsx"), "utf8")).toMatch(
    /className="p-\[13px\] screen:width-lte-700px:m-\[7px\]"/,
  );
  expect(report.warnings.length).toBe(0);
});

test("adopts identical authored definitions without duplicating them", async () => {
  const cwd = await fixture({
    entry: `@import "tailwindcss";\n\n${screenDefinition}\n${widthDefinition}`,
  });
  const report = await migrate({ cwd, extractMediaQueries: true });

  expect(report.candidates).toEqual(["p-[13px]", "screen:width-lte-700px:m-[7px]"]);
  expect(report.changedFiles).toEqual(["Button.module.css", "Button.tsx"]);
});

test("reapplying a written extraction changes nothing", async () => {
  const cwd = await fixture();
  await migrate({ cwd, extractMediaQueries: true, write: true });
  const again = await migrate({ cwd, extractMediaQueries: true, write: true });

  expect(again.changedFiles).toEqual([]);
  expect(again.diff).toBe("");
});

test("reuses a project breakpoint whose expansion matches the component", async () => {
  const cwd = await fixture({
    css: "@media screen and (min-width: 768px) { .button { margin: 7px; } }\n",
    entry: '@import "tailwindcss";\n\n@theme {\n  --breakpoint-md: 768px;\n}\n',
  });
  const report = await migrate({ cwd, extractMediaQueries: true, write: true });

  expect(report.candidates).toEqual(["screen:md:m-[7px]"]);
  // The verified breakpoint emits no definition, so the entry gains only
  // the generated `screen` block.
  expect(await readFile(join(cwd, "globals.css"), "utf8")).toBe(
    `@import "tailwindcss";\n\n@theme {\n  --breakpoint-md: 768px;\n}\n\n${screenDefinition}`,
  );
});

test("leaves media handling unchanged while extraction is disabled", async () => {
  const cwd = await fixture();
  const report = await migrate({ cwd, extractMediaQueries: false });

  expect(report.candidates).toEqual(["[@media_screen_and_(max-width:700px)]:m-[7px]", "p-[13px]"]);
  expect(report.changedFiles).toEqual(["Button.module.css", "Button.tsx"]);
});

test("does not augment a gitignored Tailwind entry discovered through HTML", async () => {
  const cwd = await tempDir();
  execFileSync("git", ["init", "-q"], { cwd });
  await Promise.all([
    writeFile(join(cwd, "package.json"), '{"private":true}'),
    writeFile(join(cwd, ".gitignore"), "globals.css\n"),
    writeFile(join(cwd, "globals.css"), '@import "tailwindcss";\n'),
    writeFile(join(cwd, "Button.module.css"), mediaCss),
    writeFile(join(cwd, "Button.tsx"), consumerTsx),
    writeFile(join(cwd, "index.html"), '<link rel="stylesheet" href="globals.css">\n'),
  ]);

  const report = await migrate({ cwd, extractMediaQueries: true, write: true });

  expect(report.changedFiles).not.toContain("globals.css");
  expect(await readFile(join(cwd, "globals.css"), "utf8")).toBe('@import "tailwindcss";\n');
  expect(report.warnings.map((warning) => warning.code)).toContain(
    "media-query-definition-fallback",
  );
});

test("an unsafe entry falls back to arbitrary variants, not legacy names", async () => {
  const cwd = await tempDir();
  execFileSync("git", ["init", "-q"], { cwd });
  await Promise.all([
    writeFile(join(cwd, "package.json"), '{"private":true}'),
    writeFile(join(cwd, ".gitignore"), "globals.css\n"),
    writeFile(join(cwd, "globals.css"), '@import "tailwindcss";\n'),
    writeFile(
      join(cwd, "Button.module.css"),
      "@media (prefers-color-scheme: dark) { .button { margin: 7px; } }\n",
    ),
    writeFile(join(cwd, "Button.tsx"), consumerTsx),
    writeFile(join(cwd, "index.html"), '<link rel="stylesheet" href="globals.css">\n'),
  ]);

  const report = await migrate({ cwd, extractMediaQueries: true, write: true });

  // Nothing about the unsafe entry's variants was verified, so even a
  // condition the legacy path would convert to `dark:` stays arbitrary.
  expect(report.candidates).toContain("[@media_(prefers-color-scheme:dark)]:m-[7px]");
  expect(report.candidates.join(" ")).not.toMatch(/(?:^| )dark:/);
});

test("an unsafe entry without media conditions warns nothing", async () => {
  const cwd = await tempDir();
  execFileSync("git", ["init", "-q"], { cwd });
  await Promise.all([
    writeFile(join(cwd, "package.json"), '{"private":true}'),
    writeFile(join(cwd, ".gitignore"), "globals.css\n"),
    writeFile(join(cwd, "globals.css"), '@import "tailwindcss";\n'),
    writeFile(join(cwd, "Button.module.css"), ".button { padding: 13px; }\n"),
    writeFile(join(cwd, "Button.tsx"), consumerTsx),
    writeFile(join(cwd, "index.html"), '<link rel="stylesheet" href="globals.css">\n'),
  ]);

  const report = await migrate({ cwd, extractMediaQueries: true, write: true });

  // There is no media behavior to preserve, so the fallback warning
  // would only be noise.
  expect(report.warnings.map((warning) => warning.code)).not.toContain(
    "media-query-definition-fallback",
  );
  expect(report.candidates).toContain("p-[13px]");
});

test("appends definitions used by retained global rules", async () => {
  const cwd = await tempDir();
  await Promise.all([
    writeFile(join(cwd, "package.json"), '{"private":true}'),
    writeFile(join(cwd, "globals.css"), '@import "tailwindcss";\n'),
    writeFile(join(cwd, "styles.css"), mediaCss),
    writeFile(
      join(cwd, "index.html"),
      '<!doctype html>\n<html>\n<head><link rel="stylesheet" href="styles.css"></head>\n<body><button class="button">Save</button></body>\n</html>\n',
    ),
  ]);
  const report = await migrate({ cwd, extractMediaQueries: true, write: true });

  expect(report.candidates).toEqual(["p-[13px]", "screen:width-lte-700px:m-[7px]"]);
  expect(report.warnings.map((warning) => warning.code)).toEqual([
    "retained-global-rule",
    "retained-global-rule",
  ]);
  expect(await readFile(join(cwd, "index.html"), "utf8")).toMatch(
    /class="button p-\[13px\] screen:width-lte-700px:m-\[7px\]"/,
  );
  expect(await readFile(join(cwd, "globals.css"), "utf8")).toBe(
    `@import "tailwindcss";\n\n${screenDefinition}\n${widthDefinition}`,
  );
});

test("probes and validates through the configured theme prefix", async () => {
  const cwd = await fixture({ entry: '@import "tailwindcss" prefix(tw);\n' });
  const report = await migrate({ cwd, extractMediaQueries: true, write: true });

  expect(report.candidates).toEqual(["tw:p-[13px]", "tw:screen:width-lte-700px:m-[7px]"]);
  expect(await readFile(join(cwd, "globals.css"), "utf8")).toBe(
    `@import "tailwindcss" prefix(tw);\n\n${screenDefinition}\n${widthDefinition}`,
  );
});

test("never reuses a variant that re-scopes the media condition", async () => {
  const cwd = await fixture({
    css: "@media (prefers-color-scheme: dark) { .button { margin: 7px; } }\n",
    entry:
      '@import "tailwindcss";\n\n@custom-variant dark {\n  @media (prefers-color-scheme: dark) {\n    &:where(.dark *) {\n      @slot;\n    }\n  }\n}\n',
  });
  const report = await migrate({ cwd, extractMediaQueries: true, write: true });

  expect(report.candidates).toEqual(["prefers-color-scheme-dark:m-[7px]"]);
  expect(await readFile(join(cwd, "globals.css"), "utf8")).toMatch(
    /@custom-variant prefers-color-scheme-dark \{\n {2}@media \(prefers-color-scheme: dark\) \{\n {4}@slot;\n {2}\}\n\}\n$/,
  );
});

test("orders definitions by applied rules, not blocked occurrences", () => {
  const rule = (candidates: string[], status: PlanRule["status"]): PlanRule => ({
    selector: ".x",
    status,
    candidates,
    file: "styles.css",
    ruleId: { start: 0, end: 0 },
    authoredSpan: { start: 0, end: 0 },
    stylesheet: 0,
  });
  // The blocked first rule shares `b:m-[7px]` with the last applied rule;
  // its dropped `broken:p-[1px]` proves it never applied, so registration
  // order must follow the applied rules: `a` before `b`.
  const plan: Plan = {
    files: [],
    deletedFiles: [],
    unlinkedFiles: [],
    candidates: ["a:m-[7px]", "b:m-[7px]"],
    rules: [
      rule(["b:m-[7px]", "broken:p-[1px]"], "retained"),
      rule(["a:m-[7px]"], "converted"),
      rule(["b:m-[7px]"], "converted"),
    ],
    warnings: [],
    convertedRules: 2,
    retainedRules: 1,
  };
  const extraction = {
    names: { "(width <= 700px)": "a", "(width <= 800px)": "b" },
    generated: [
      { key: "(width <= 700px)", name: "a" },
      { key: "(width <= 800px)", name: "b" },
    ],
  };

  expect(usedGeneratedDefinitions(extraction, plan)).toEqual([
    { key: "(width <= 700px)", name: "a" },
    { key: "(width <= 800px)", name: "b" },
  ]);
});
