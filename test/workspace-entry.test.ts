import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { expect, onTestFinished, test, vi } from "vite-plus/test";

import { migrate } from "../src/index.ts";
import * as tailwind from "../src/tailwind.ts";
import * as canonicalize from "../src/plan/canonicalize.ts";

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

test("shares stylesheet reservations across entries only within one migration", async () => {
  const scan = vi.spyOn(canonicalize, "spellingReservations");
  onTestFinished(() => scan.mockRestore());
  const files: Record<string, string> = { "package.json": '{"private":true}' };
  for (const name of ["a", "b"]) {
    Object.assign(files, app(name, ".button { padding: var(--space); }\n"), {
      [`packages/${name}/globals.css`]: '@import "tailwindcss";\n',
      [`packages/${name}/main.tsx`]:
        'import "./globals.css"; import { Button } from "./Button.tsx"; export const render = Button;\n',
    });
  }
  const cwd = await workspace(files);
  const preview = await migrate({ cwd, workspaces: true });
  expect(preview.convertedRules).toBe(2);
  expect(preview.failures).toEqual([]);
  expect(scan).toHaveBeenCalledTimes(2);
  const firstCache = scan.mock.calls[0][3];
  expect(firstCache).toBeDefined();
  expect(scan.mock.calls[1][3]).toBe(firstCache);
  expect(firstCache?.size).toBe(1);

  await writeFile(join(cwd, "packages/a/unused.module.css"), ".late { --late: 1; }\n");
  const next = await migrate({ cwd, workspaces: true, force: true });
  expect(next.failures).toEqual([]);
  const nextCache = scan.mock.calls[2][3];
  expect(nextCache).toBeDefined();
  expect(nextCache).not.toBe(firstCache);
  expect(scan.mock.calls[3][3]).toBe(nextCache);
  expect([...nextCache!.values()][0].properties.has("--late")).toBe(true);
  expect([...firstCache!.values()][0].properties.has("--late")).toBe(false);
  expect(await readFile(join(cwd, "packages/a/Button.module.css"), "utf8")).toBe(
    files["packages/a/Button.module.css"],
  );
  expect(await migrate({ cwd, workspaces: true, force: true, write: true })).toEqual(next);
});

test("rejects malformed shared-entry metadata without guessing package privacy", async () => {
  const css = ".button { padding: 13px; }\n";
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app", css),
    "packages/app/package.json": '{"private":"true","exports":{".":["./main.tsx",null]}}',
  });
  await expect(migrate({ cwd, workspaces: true, write: true })).rejects.toThrow(/private/);
  expect(await readFile(join(cwd, "packages/app/Button.module.css"), "utf8")).toBe(css);
});

test("an entry group shares one allocation and one composed entry edit", async () => {
  const originalLoad = tailwind.loadTailwind;
  const reload = vi.fn();
  const load = vi.spyOn(tailwind, "loadTailwind").mockImplementation(async (...args) => {
    const entry = await originalLoad(...args);
    return {
      ...entry,
      loadWith: async (css) => {
        reload(css);
        return entry.loadWith(css);
      },
    };
  });
  onTestFinished(() => load.mockRestore());
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app"),
    ...app("lib"),
  });
  const preview = await migrate({ cwd, workspaces: true });
  expect(load).toHaveBeenCalledTimes(1);
  expect(load.mock.calls[0].slice(0, 2)).toEqual([cwd, join(cwd, "globals.css")]);
  expect(reload).toHaveBeenCalledWith(expect.stringContaining("@custom-variant width-lte-700px"));
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });
  expect(load).toHaveBeenCalledTimes(2);
  expect(report).toEqual(preview);

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

test.each(
  ["globals.css", "theme.css", "mutate.cjs"].flatMap((input) =>
    [false, true].flatMap((write) => [false, true].map((force) => ({ input, write, force }))),
  ),
)(
  "rejects changed shared Tailwind input $input before reuse with write=$write and force=$force",
  async ({ input, write, force }) => {
    const load = vi.spyOn(tailwind, "loadTailwind");
    onTestFinished(() => load.mockRestore());
    const files = {
      "package.json": '{"private":true}',
      "globals.css": '@import "tailwindcss";\n@import "./theme.css";\n@plugin "./mutate.cjs";\n',
      "theme.css": "@theme { --color-brand: red; }\n",
      "mutate.cjs": `module.exports = () => {
        require("node:fs").appendFileSync(
          require("node:path").join(__dirname, ${JSON.stringify(input)}),
          "\\n/* changed during planning */\\n",
        );
      };\n`,
      ...app("app", ".button { padding: 13px; }\n"),
      ...app("lib", ".button { padding: 13px; }\n"),
    };
    const cwd = await workspace(files);

    await expect(migrate({ cwd, workspaces: true, write, force })).rejects.toThrow(
      `Source changed during planning: ${join(cwd, input)}`,
    );
    expect(load).toHaveBeenCalledTimes(1);
    for (const [path, source] of Object.entries(files)) {
      expect(await readFile(join(cwd, path), "utf8")).toBe(
        path === input ? `${source}\n/* changed during planning */\n` : source,
      );
    }
  },
);

test("loads separate Tailwind entries from their own packages", async () => {
  const load = vi.spyOn(tailwind, "loadTailwind");
  onTestFinished(() => load.mockRestore());
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app", ".button { padding: 13px; }\n"),
    ...app("other", ".button { padding: 13px; }\n"),
    "packages/other/globals.css": '@import "tailwindcss";\n',
    "packages/other/main.tsx":
      "import './globals.css';\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  });
  const report = await migrate({ cwd, workspaces: true });
  expect(report.convertedRules).toBe(2);
  expect(report.failures).toEqual([]);
  expect(load.mock.calls.map((args) => args.slice(0, 2))).toEqual([
    [cwd, join(cwd, "globals.css")],
    [join(cwd, "packages/other"), join(cwd, "packages/other/globals.css")],
  ]);
});

test("does not reuse a failed shared Tailwind load under force", async () => {
  const load = vi
    .spyOn(tailwind, "loadTailwind")
    .mockRejectedValueOnce(new Error("Tailwind load failed"));
  onTestFinished(() => load.mockRestore());
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app", ".button { padding: 13px; }\n"),
    ...app("lib", ".button { padding: 13px; }\n"),
  });
  const report = await migrate({ cwd, workspaces: true, force: true });
  expect(load).toHaveBeenCalledTimes(2);
  expect(report.convertedRules).toBe(1);
  expect(report.failures).toEqual([{ package: "packages/app", message: "Tailwind load failed" }]);
});

test.each([
  "https://fonts.googleapis.com/css2?family=Inter&display=swap",
  "//fonts.googleapis.com/css2?family=Inter&display=swap",
  "HTTPS://example.invalid/fonts.css",
  "hTtP://example.invalid/fonts.css",
  "DATA:text/css,@source%20none;",
])("resolves a shared Tailwind entry with external imports: %s", async (url) => {
  const entry = `@import url("${url}");\n@import "tailwindcss";\n`;
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": entry,
    ...app("app", ".button { padding: 13px; }\n"),
  });

  const report = await migrate({ cwd, workspaces: true });

  expect(report.failures).toEqual([]);
  expect(report.changedFiles).toEqual([
    "packages/app/Button.module.css",
    "packages/app/Button.tsx",
  ]);
  expect(report.candidates).toEqual(["p-[13px]"]);
  expect(await readFile(join(cwd, "globals.css"), "utf8")).toBe(entry);
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

test("reports stylesheets rejected by the shared-entry proof", async () => {
  const files: Record<string, string> = {
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app"),
  };
  // The stray consumer is unreachable from the loading source, so its
  // stylesheet is retained and reported while the proven one migrates.
  files["packages/app/Stray.tsx"] =
    "import styles from './Stray.module.css';\nexport const Stray = () => <i className={styles.stray} />;\n";
  files["packages/app/Stray.module.css"] = ".stray { padding: 13px; }\n";
  const cwd = await workspace(files);
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  const flow = report.warnings.filter((warning) => warning.code === "unproven-shared-entry-flow");
  expect(flow).toHaveLength(1);
  expect(flow[0].file).toBe("packages/app/Stray.module.css");
  expect(await readFile(join(cwd, "packages/app/Stray.module.css"), "utf8")).toContain(".stray");
  expect(await readFile(join(cwd, "packages/app/Button.tsx"), "utf8")).toMatch(
    /screen:width-lte-700px:m-\[7px\]/,
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

test("returns proof warnings when every target is rejected", async () => {
  const files: Record<string, string> = {
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app"),
  };
  // The loader no longer reaches the consumer, so every stylesheet is
  // rejected and the package must still surface the retention warnings.
  files["packages/app/main.tsx"] = "import '../../globals.css';\n";
  const cwd = await workspace(files);
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true });

  const flow = report.warnings.filter((warning) => warning.code === "unproven-shared-entry-flow");
  expect(flow).toHaveLength(1);
  expect(flow[0].file).toBe("packages/app/Button.module.css");
  expect(report.changedFiles).toEqual([]);
});

test("composes edits when one component consumes two members' stylesheets", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    "shared.module.css": ".frame { margin: 7px; }\n",
    "main.tsx": "import './globals.css';\n",
    ...app("app", ".button { padding: 13px; }\n"),
  });
  // The child component consumes the root-owned module and its own module,
  // so both members edit the same file and the group must compose the
  // edits instead of aborting on a duplicate path claim.
  await writeFile(
    join(cwd, "packages/app/Button.tsx"),
    "import shared from '../../shared.module.css';\nimport styles from './Button.module.css';\nexport const Button = () => <button className={`${shared.frame} ${styles.button}`}>Save</button>;\n",
  );
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  const button = await readFile(join(cwd, "packages/app/Button.tsx"), "utf8");
  expect(button).toContain("m-[7px]");
  expect(button).toContain("p-[13px]");
  expect(report.failures).toEqual([]);
});

test("skips only the member whose media collection fails", async () => {
  const files: Record<string, string> = {
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app"),
    ...app("lib"),
  };
  files["packages/lib/Button.module.css"] = ".broken { color: red }}}\n";
  const cwd = await workspace(files);
  const report = await migrate({
    cwd,
    workspaces: true,
    extractMediaQueries: true,
    force: true,
    write: true,
  });

  expect(report.failures).toHaveLength(1);
  expect(report.failures[0].package).toBe("packages/lib");
  expect(await readFile(join(cwd, "packages/app/Button.tsx"), "utf8")).toMatch(
    /screen:width-lte-700px:m-\[7px\]/,
  );
});

test("composes html link removals across members on one page", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    "shared.module.css": ".rootbox { padding: 13px; }\n",
    "packages/site/package.json": '{"private":true}',
    "packages/site/app.module.css": ".appbox { margin: 7px; }\n",
    "packages/site/index.html":
      '<!doctype html>\n<html>\n<head><link rel="stylesheet" href="../../globals.css"><link rel="stylesheet" href="../../shared.module.css"><link rel="stylesheet" href="app.module.css"></head>\n<body><div class="rootbox appbox">Hi</div></body>\n</html>\n',
  });
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  // Both members unlink their module from the same page, so the removals
  // must compose into one final claim instead of aborting the merge.
  const html = await readFile(join(cwd, "packages/site/index.html"), "utf8");
  expect(html).toContain('<head><link rel="stylesheet" href="../../globals.css"></head>');
  expect(html).toMatch(/class="rootbox appbox m-\[7px\] p-\[13px\]"/);
  expect(report.failures).toEqual([]);
});

test("retains duplicate at-rule registrations within one member", async () => {
  const property = (value: string, klass: string) =>
    `@property --brand { syntax: "<color>"; inherits: false; initial-value: ${value}; }\n.${klass} { padding: 13px; }\n`;
  const files: Record<string, string> = {
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app", property("red", "button")),
  };
  files["packages/app/Card.module.css"] = property("blue", "card");
  files["packages/app/Card.tsx"] =
    "import styles from './Card.module.css';\nexport const Card = () => <div className={styles.card} />;\n";
  files["packages/app/main.tsx"] =
    "import '../../globals.css';\nimport { Button } from './Button.tsx';\nimport { Card } from './Card.tsx';\nexport const render = [Button, Card];\n";
  const cwd = await workspace(files);
  await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  // Two stylesheets of one package move the same registration with
  // different content; their load order is unproven, so neither moves.
  expect(await readFile(join(cwd, "globals.css"), "utf8")).not.toContain("@property");
  expect(await readFile(join(cwd, "packages/app/Button.module.css"), "utf8")).toContain(
    "@property --brand",
  );
  expect(await readFile(join(cwd, "packages/app/Card.module.css"), "utf8")).toContain(
    "@property --brand",
  );
});

test("rebases later member edits after earlier members shift offsets", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    "shared.module.css": ".rootbox { padding: 13px; }\n",
    "packages/site/package.json": '{"private":true}',
    "packages/site/app.module.css": ".appbox { margin: 7px; }\n",
    "packages/site/index.html":
      '<!doctype html>\n<html>\n<head><link rel="stylesheet" href="../../globals.css"><link rel="stylesheet" href="../../shared.module.css"><link rel="stylesheet" href="app.module.css"></head>\n<body><div class="rootbox">A</div><div class="appbox">B</div></body>\n</html>\n',
  });
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  // The root member's edit lengthens the page before the site member's
  // element, so the second edit must rebase through the applied history.
  const html = await readFile(join(cwd, "packages/site/index.html"), "utf8");
  expect(html).toContain('<div class="rootbox p-[13px]">A</div>');
  expect(html).toContain('<div class="appbox m-[7px]">B</div>');
  expect(report.failures).toEqual([]);
});

test("a commented import is not a consumer for shared-entry proofs", async () => {
  const files: Record<string, string> = {
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app"),
  };
  // The only mention of the orphan module is a commented-out import, so
  // it has no proven consumer and its at-rules must not activate in the
  // shared entry.
  files["packages/app/Fonts.module.css"] =
    '@font-face { font-family: "Acme"; src: url(/acme.woff2); }\n';
  files["packages/app/main.tsx"] =
    "import '../../globals.css';\n// import fonts from './Fonts.module.css';\nimport { Button } from './Button.tsx';\nexport const render = Button;\n";
  const cwd = await workspace(files);
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  expect(await readFile(join(cwd, "globals.css"), "utf8")).not.toContain("@font-face");
  expect(await readFile(join(cwd, "packages/app/Fonts.module.css"), "utf8")).toContain(
    "@font-face",
  );
  expect(
    report.warnings.some(
      (warning) =>
        warning.code === "unproven-shared-entry-flow" &&
        warning.file === "packages/app/Fonts.module.css",
    ),
  ).toBe(true);
});

test("owner styles require proofs for their cross-package consumers", async () => {
  const files: Record<string, string> = {
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    "shared.module.css": ".frame { margin: 7px; }\n",
    "main.tsx": "import './globals.css';\n",
    ...app("app"),
  };
  // Stray.tsx consumes the root-owned module but is unreachable from the
  // child's loading flows, so the root stylesheet must be retained.
  files["packages/app/Stray.tsx"] =
    "import shared from '../../shared.module.css';\nexport const Stray = () => <i className={shared.frame} />;\n";
  const cwd = await workspace(files);
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  expect(await readFile(join(cwd, "shared.module.css"), "utf8")).toContain(".frame");
  expect(await readFile(join(cwd, "packages/app/Stray.tsx"), "utf8")).toContain("shared.frame");
  expect(
    report.warnings.some(
      (warning) =>
        warning.code === "unproven-shared-entry-flow" && warning.file === "shared.module.css",
    ),
  ).toBe(true);
  // The child's own proven stylesheet still migrates.
  expect(await readFile(join(cwd, "packages/app/Button.tsx"), "utf8")).toMatch(
    /screen:width-lte-700px:m-\[7px\]/,
  );
});

test("retains overlapping page registrations across members", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app", "@page { margin: 1cm; }\n.button { padding: 13px; }\n"),
    ...app("lib", "@page :left { margin: 2cm; }\n.button { padding: 13px; }\n"),
  });
  await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  // A bare @page overlaps the :left variant, so neither may move: the
  // composed order cannot prove the original winner on left pages.
  expect(await readFile(join(cwd, "globals.css"), "utf8")).not.toContain("@page");
  expect(await readFile(join(cwd, "packages/app/Button.module.css"), "utf8")).toContain("@page");
  expect(await readFile(join(cwd, "packages/lib/Button.module.css"), "utf8")).toContain(
    "@page :left",
  );
});

test("url-imported entry sheets join registration collision checks", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n@import url(./base.css);\n',
    "base.css": '@property --brand { syntax: "<color>"; inherits: false; initial-value: red; }\n',
    ...app(
      "app",
      '@property --brand { syntax: "<color>"; inherits: false; initial-value: blue; }\n.button { padding: 13px; }\n',
    ),
  });
  await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  // The url-imported sheet already registers --brand, so the member's
  // conflicting registration must stay in its module.
  expect(await readFile(join(cwd, "globals.css"), "utf8")).not.toContain("@property");
  expect(await readFile(join(cwd, "packages/app/Button.module.css"), "utf8")).toContain(
    "@property --brand",
  );
});

test("keeps active global at-rules of shared members in their modules", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app(
      "app",
      '@font-face { font-family: "Acme"; src: url(/acme.woff2); }\n.button { padding: 13px; }\n',
    ),
  });
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  // Appending the font to the ancestor-shared entry would activate it in
  // every other flow loading that entry, so it stays in the module while
  // the rule itself still migrates.
  expect(await readFile(join(cwd, "globals.css"), "utf8")).not.toContain("@font-face");
  expect(await readFile(join(cwd, "packages/app/Button.module.css"), "utf8")).toContain(
    "@font-face",
  );
  expect(await readFile(join(cwd, "packages/app/Button.tsx"), "utf8")).toContain("p-[13px]");
  expect(report.failures).toEqual([]);
});

test("owner moves collide with registrations retained in shared members", async () => {
  const property = (value: string) =>
    `@property --brand { syntax: "<color>"; inherits: false; initial-value: ${value}; }\n.frame { padding: 13px; }\n`;
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    "shared.module.css": property("red"),
    "main.tsx":
      "import './globals.css';\nimport shared from './shared.module.css';\nexport const render = shared.frame;\n",
    ...app("app", property("blue")),
  });
  await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  // The shared child retains its registration in place, so the owner's
  // move of the same identity would flip precedence and must be retained.
  expect(await readFile(join(cwd, "globals.css"), "utf8")).not.toContain("@property");
  expect(await readFile(join(cwd, "shared.module.css"), "utf8")).toContain("@property --brand");
  expect(await readFile(join(cwd, "packages/app/Button.module.css"), "utf8")).toContain(
    "@property --brand",
  );
});

test("owner moves collide with registrations retained in plain stylesheets", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    "site.css":
      '@property --brand { syntax: "<color>"; inherits: false; initial-value: red; }\n.rootbox { color: var(--brand); }\n',
    "shared.module.css":
      '@property --brand { syntax: "<color>"; inherits: false; initial-value: blue; }\n.frame { padding: 13px; }\n',
    "main.tsx":
      "import './globals.css';\nimport './site.css';\nimport shared from './shared.module.css';\nexport const render = shared.frame;\n",
  });
  await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  // The plain stylesheet keeps its registration in place, so moving the
  // module's conflicting registration into the entry would flip which one
  // wins; the module must retain it.
  expect(await readFile(join(cwd, "globals.css"), "utf8")).not.toContain("@property");
  expect(await readFile(join(cwd, "shared.module.css"), "utf8")).toContain("@property --brand");
});

test("same-named modules across packages keep distinct keyframe scopes", async () => {
  const keyframes = (opacity: string) =>
    `@keyframes fade { from { opacity: ${opacity}; } to { opacity: 1; } }\n.button { animation: fade 1s; }\n`;
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    ...app("app", keyframes("0")),
    ...app("lib", keyframes("0.5")),
  });
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: true, write: true });

  // Both packages define `fade` in a module named Button.module.css; the
  // workspace-relative scope keeps their migrated names distinct.
  const entry = await readFile(join(cwd, "globals.css"), "utf8");
  expect(entry.match(/@keyframes tw-migrate-[0-9a-f]+-fade/g)).toHaveLength(2);
  expect(report.failures).toEqual([]);
});

test("attributes malformed inputs with extraction disabled", async () => {
  const cwd = await workspace({
    "package.json": '{"private":true}',
    "globals.css": '@import "tailwindcss";\n',
    "Button.module.css": ".broken { color: red }}}\n",
    "Button.tsx":
      "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n",
  });
  const report = await migrate({ cwd, extractMediaQueries: false, force: true, write: true });

  // The collection pass runs for isolation even when extraction is off,
  // so the malformed stylesheet stays a package-attributed failure.
  expect(report.failures).toHaveLength(1);
  expect(await readFile(join(cwd, "Button.module.css"), "utf8")).toContain(".broken");
});
