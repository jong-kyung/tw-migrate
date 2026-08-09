import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { onTestFinished, test } from "vite-plus/test";

import { migrate } from "../src/index.ts";

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

  assert.deepEqual(report.candidates, ["p-[13px]", "screen:width-lte-700px:m-[7px]"]);
  assert.deepEqual(report.changedFiles, ["Button.module.css", "Button.tsx", "globals.css"]);
  assert.equal(
    await readFile(join(cwd, "globals.css"), "utf8"),
    `@import "tailwindcss";\n\n${screenDefinition}\n${widthDefinition}`,
  );
  assert.match(
    await readFile(join(cwd, "Button.tsx"), "utf8"),
    /className="p-\[13px\] screen:width-lte-700px:m-\[7px\]"/,
  );
  assert.equal(report.warnings.length, 0);
});

test("adopts identical authored definitions without duplicating them", async () => {
  const cwd = await fixture({
    entry: `@import "tailwindcss";\n\n${screenDefinition}\n${widthDefinition}`,
  });
  const report = await migrate({ cwd, extractMediaQueries: true });

  assert.deepEqual(report.candidates, ["p-[13px]", "screen:width-lte-700px:m-[7px]"]);
  assert.deepEqual(report.changedFiles, ["Button.module.css", "Button.tsx"]);
});

test("reapplying a written extraction changes nothing", async () => {
  const cwd = await fixture();
  await migrate({ cwd, extractMediaQueries: true, write: true });
  const again = await migrate({ cwd, extractMediaQueries: true, write: true });

  assert.deepEqual(again.changedFiles, []);
  assert.equal(again.diff, "");
});

test("reuses a project breakpoint whose expansion matches the component", async () => {
  const cwd = await fixture({
    css: "@media screen and (min-width: 768px) { .button { margin: 7px; } }\n",
    entry: '@import "tailwindcss";\n\n@theme {\n  --breakpoint-md: 768px;\n}\n',
  });
  const report = await migrate({ cwd, extractMediaQueries: true, write: true });

  assert.deepEqual(report.candidates, ["screen:md:m-[7px]"]);
  // The verified breakpoint emits no definition, so the entry gains only
  // the generated `screen` block.
  assert.equal(
    await readFile(join(cwd, "globals.css"), "utf8"),
    `@import "tailwindcss";\n\n@theme {\n  --breakpoint-md: 768px;\n}\n\n${screenDefinition}`,
  );
});

test("leaves media handling unchanged while extraction is disabled", async () => {
  const cwd = await fixture();
  const report = await migrate({ cwd });

  assert.deepEqual(report.candidates, [
    "[@media_screen_and_(max-width:700px)]:m-[7px]",
    "p-[13px]",
  ]);
  assert.deepEqual(report.changedFiles, ["Button.module.css", "Button.tsx"]);
});
