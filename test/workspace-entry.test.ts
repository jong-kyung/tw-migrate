import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { expect, onTestFinished, test } from "vite-plus/test";

import { migrate } from "../src/index.ts";

const mediaCss =
  ".button { padding: 13px; }\n@media screen and (max-width: 700px) { .button { margin: 7px; } }\n";

async function workspace(files: Record<string, string>): Promise<string> {
  await mkdir(".tmp", { recursive: true });
  const cwd = await mkdtemp(join(process.cwd(), ".tmp", "fixture-"));
  onTestFinished(() => rm(cwd, { recursive: true, force: true }));
  for (const [path, source] of Object.entries(files)) {
    await mkdir(dirname(join(cwd, path)), { recursive: true });
    await writeFile(join(cwd, path), source);
  }
  return cwd;
}

function app(name: string, css: string = mediaCss): Record<string, string> {
  return {
    [`packages/${name}/package.json`]: '{"private":true}',
    [`packages/${name}/main.tsx`]:
      "import '../../globals.css';\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
    [`packages/${name}/Button.tsx`]:
      "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n",
    [`packages/${name}/Button.module.css`]: css,
  };
}

test("an entry group shares one allocation and one composed entry edit", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app"),
    ...app("lib"),
  });
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  expect(report.changedFiles).toEqual([
    "globals.css",
    "packages/app/Button.module.css",
    "packages/app/Button.tsx",
    "packages/lib/Button.module.css",
    "packages/lib/Button.tsx",
  ]);
  // Both packages resolve the same condition to the same names, and the
  // shared entry gains each definition exactly once.
  for (const name of ["app", "lib"]) {
    expect(await readFile(join(cwd, `packages/${name}/Button.tsx`), "utf8")).toMatch(
      /className="p-\[13px\] screen:width-lte-700px:m-\[7px\]"/,
    );
  }
  const entry = await readFile(join(cwd, "globals.css"), "utf8");
  expect(entry.match(/@custom-variant screen /g)).toHaveLength(1);
  expect(entry.match(/@custom-variant width-lte-700px /g)).toHaveLength(1);

  const again = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });
  expect(again.changedFiles).toEqual([]);
});

test("composes moved keyframes from group members into the shared entry", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app(
      "app",
      "@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n.button { animation: fade 1s; }\n",
    ),
    ...app("lib"),
  });
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  const entry = await readFile(join(cwd, "globals.css"), "utf8");
  expect(entry).toMatch(/@keyframes tw-migrate-[0-9a-f]+-fade/);
  expect(entry.match(/@custom-variant screen /g)).toHaveLength(1);
  expect(report.changedFiles).toContain("globals.css");
});

test("retains colliding order-sensitive at-rule moves across members", async () => {
  const property = (value: string) =>
    `@property --brand { syntax: "<color>"; inherits: false; initial-value: ${value}; }\n.button { padding: 13px; }\n`;
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app", property("red")),
    ...app("lib", property("blue")),
  });
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  // The two @property registrations share one name with different content,
  // so neither may move: appending would pick an unproven winner.
  expect(await readFile(join(cwd, "globals.css"), "utf8")).not.toContain("@property");
  expect(await readFile(join(cwd, "packages/app/Button.module.css"), "utf8")).toContain(
    "@property --brand",
  );
  expect(await readFile(join(cwd, "packages/lib/Button.module.css"), "utf8")).toContain(
    "@property --brand",
  );
  expect(report.retainedRules).toBeGreaterThan(0);
});

test("skips an unproven child package only under force", async () => {
  const files: Record<string, string> = {
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app"),
    ...app("lib"),
  };
  // The lib package no longer loads the entry, so its ancestor relationship
  // is unproven and entry resolution fails for it.
  files["packages/lib/main.tsx"] =
    "import { Button } from './Button.tsx';\nexport const render = Button;\n";
  const strict = await workspace(files);
  await expect(
    migrate({ cwd: strict, workspaces: true, extractMediaQueries: true }),
  ).rejects.toThrow(/No Tailwind v4 CSS entry was found/);

  const forced = await workspace(files);
  const report = await migrate({
    cwd: forced,
    workspaces: true,
    extractMediaQueries: true,
    force: true,
    write: true,
  });
  expect(report.failures).toHaveLength(1);
  expect(report.failures[0].package).toBe("packages/lib");
  expect(await readFile(join(forced, "packages/app/Button.tsx"), "utf8")).toMatch(
    /screen:width-lte-700px:m-\[7px\]/,
  );
  expect(await readFile(join(forced, "packages/lib/Button.tsx"), "utf8")).toContain(
    "styles.button",
  );
});

test("an html child package proves the ancestor entry through its link", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    "packages/site/package.json": '{"private":true}',
    "packages/site/index.html":
      '<!doctype html>\n<html>\n<head><link rel="stylesheet" href="../../globals.css"><link rel="stylesheet" href="styles.css"></head>\n<body><button class="button">Save</button></body>\n</html>\n',
    "packages/site/styles.css": mediaCss,
  });
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  expect(await readFile(join(cwd, "packages/site/index.html"), "utf8")).toMatch(
    /class="button p-\[13px\] screen:width-lte-700px:m-\[7px\]"/,
  );
  expect(await readFile(join(cwd, "globals.css"), "utf8")).toContain("@custom-variant screen ");
  expect(report.failures).toEqual([]);
});
