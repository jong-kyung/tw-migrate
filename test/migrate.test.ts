import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  lstat,
  mkdtemp,
  mkdir,
  readFile,
  readdir,
  rm,
  symlink,
  unlink,
  writeFile,
} from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { pathToFileURL } from "node:url";
import { onTestFinished, test, vi } from "vite-plus/test";

import { __unstable__loadDesignSystem as loadDesignSystem } from "tailwindcss";

import { migrate } from "../src/index.ts";
import {
  collectMediaConditions,
  cssDirectives,
  decodeSourceMap,
  expressionAnalysis,
  mediaProbeKey,
  planBatchMigration,
  sourceAnalysis,
  stylesheetAnalysis,
  validateCss,
} from "../src/native.ts";
import { compileSassEntry, loadProjectSass, sourceMappings } from "../src/parser/style-compiler.ts";
import { writeChanges } from "../src/util/write.ts";

// Full migrations replan with candidate canonicalization, whose first
// design-system lookup builds Tailwind's utility index; slower CI runners
// exceed the 5s default by a wide margin.
vi.setConfig({ testTimeout: 60000 });

const initialCss = ".button { padding: 13px; }\n";
const initialTsx =
  "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n";
const recoverablePrefix = "TW_MIGRATE_RECOVERABLE_INPUT:";
const malformedCss = "@media \u000bscreen {}";

function assertNativeError(run: () => unknown, message: string) {
  assert.throws(run, (error: unknown) => {
    assert(error instanceof Error);
    assert.equal(error.message, message);
    return true;
  });
}

test("native endpoint error routing stays byte-exact", () => {
  assertNativeError(
    () => validateCss(malformedCss),
    'Edited stylesheet no longer parses: Error { kind: Unexpected("<ident>", "<unknown>"), span: Span { start: 7, end: 8 } }',
  );
  assertNativeError(
    () => expressionAnalysis("app.js", "const ="),
    'Failed to parse app.js: Diagnostics([OxcDiagnostic { inner: OxcDiagnosticInner { message: "Unexpected token", labels: One([LabeledSpan { label: None, span: SourceSpan { offset: SourceOffset(0), length: 5 }, primary: false }]), help: None, note: None, severity: Error, code: OxcCode { scope: None, number: None }, url: None } }])',
  );
  assertNativeError(() => sourceAnalysis("app.js", "const ="), "Failed to parse app.js");
  assertNativeError(
    () => stylesheetAnalysis("app.css", malformedCss),
    'Failed to parse app.css: Error { kind: Unexpected("<ident>", "<unknown>"), span: Span { start: 7, end: 8 } }',
  );
  assert.equal(cssDirectives(malformedCss), null);
  assertNativeError(
    () => mediaProbeKey(malformedCss),
    'Failed to parse probe CSS: Error { kind: Unexpected("<ident>", "<unknown>"), span: Span { start: 7, end: 8 } }',
  );
  assertNativeError(
    () =>
      collectMediaConditions(
        JSON.stringify({ stylesheets: [{ cssPath: "app.css", cssSource: malformedCss }] }),
      ),
    `${recoverablePrefix}Failed to parse app.css: Error { kind: Unexpected("<ident>", "<unknown>"), span: Span { start: 7, end: 8 } }`,
  );
});

test("batch planning prefixes malformed Tailwind entry CSS", () => {
  const request = {
    tailwindPath: "tailwind.css",
    tailwindSource: malformedCss,
    files: [],
    stylesheets: [
      {
        cssPath: "tokens.module.css",
        cssSource: '@property --x { syntax: "*"; inherits: false; initial-value: 0; }',
        isModule: true,
      },
    ],
  };

  assertNativeError(
    () => planBatchMigration(JSON.stringify(request)),
    `${recoverablePrefix}Failed to parse Tailwind CSS: Error { kind: Unexpected("<ident>", "<unknown>"), span: Span { start: 7, end: 8 } }`,
  );
});

test("source-map decoding failures stay fatal and unprefixed", () => {
  assertNativeError(
    () => decodeSourceMap("{"),
    "Failed to decode source map: JSON parsing error: EOF while parsing an object at line 1 column 1",
  );
});

async function tempDir(): Promise<string> {
  await mkdir(".tmp", { recursive: true });
  const cwd = await mkdtemp(join(process.cwd(), ".tmp", "fixture-"));
  onTestFinished(() => rm(cwd, { recursive: true, force: true }));
  return cwd;
}

async function fixture({ css = initialCss, tsx = initialTsx } = {}) {
  const cwd = await tempDir();
  await Promise.all([
    writeFile(join(cwd, "package.json"), '{"private":true}'),
    writeFile(join(cwd, "globals.css"), '@import "tailwindcss";\n'),
    writeFile(join(cwd, "Button.module.css"), css),
    writeFile(join(cwd, "Button.tsx"), tsx),
  ]);
  return cwd;
}

test("canonicalizes aliased cwd paths before Git discovery", async () => {
  const cwd = await fixture();
  const alias = `${cwd}-alias`;
  let linked = false;
  try {
    execFileSync("git", ["init", "-q"], { cwd });
    await symlink(cwd, alias, process.platform === "win32" ? "junction" : "dir");
    linked = true;
    const report = await migrate({ cwd: alias });
    assert.deepEqual(report.changedFiles, ["Button.module.css", "Button.tsx"]);
  } finally {
    if (linked) await unlink(alias);
  }
});

test("ignores malformed scan-only HTML without stylesheet links", async () => {
  const cwd = await fixture();
  execFileSync("git", ["init", "-q"], { cwd });
  await Promise.all([
    writeFile(join(cwd, ".gitignore"), "ignored.html\n"),
    writeFile(join(cwd, "ignored.html"), '<div id="a" id="b"></div>\n'),
  ]);
  const report = await migrate({ cwd });
  assert.equal(report.convertedRules, 1);
  assert.deepEqual(report.failures, []);
});

test("returns structured migration report fields", async () => {
  const cwd = await fixture();
  const report = await migrate({ cwd });
  assert.deepEqual(report.candidates, ["p-[13px]"]);
  assert.deepEqual(report.rules, [
    {
      selector: ".button",
      status: "converted",
      candidates: ["p-[13px]"],
      file: "Button.module.css",
      ruleId: { start: 0, end: 26 },
      authoredSpan: { start: 0, end: 26 },
    },
  ]);
});

test("reports warning line and column while converting independent rules", async () => {
  const template = `import styles from './Button.module.css';
export const Button = ({ active }) => (
  <>
    <button aria-label="저장 😀" className={
      getClass(active, styles.blocked)
    }>Blocked</button>
    <button className={styles.button}>Safe</button>
  </>
);
`;

  for (const [lineEnding, expected] of [
    ["\r\n", [4, 42, 6, 6]],
    ["\r", [4, 42, 6, 6]],
    ["\u2028", [4, 42, 6, 6]],
    ["\u2029", [4, 42, 6, 6]],
    ["\r\u2028", [7, 42, 11, 6]],
    ["\r\u2029", [7, 42, 11, 6]],
  ] as const) {
    const cwd = await fixture({
      css: ".blocked { color: red; }\n.button { padding: 13px; }\n",
      tsx: template.replaceAll("\n", lineEnding),
    });
    const report = await migrate({ cwd });
    const warning = report.warnings.find((entry) => entry.code === "dynamic-class-name")!;

    assert.equal(report.convertedRules, 1);
    assert.equal(report.retainedRules, 1);
    assert.deepEqual([warning.line, warning.column, warning.endLine, warning.endColumn], expected);
  }
});

test("counts CSS form feeds in warning locations", async () => {
  const cwd = await fixture({
    css: ".unused { padding: 13px; }\f.button { color: v-bind(theme); }\n",
  });

  const report = await migrate({ cwd });
  const warning = report.warnings.find((entry) => entry.code === "unsupported-value")!;

  assert.equal(warning.line, 2);
});

test("keeps distinct conflict warnings at the same source span", async () => {
  const cwd = await fixture({
    css: ".a { padding: 13px; }\n",
    tsx: `import a from './Button.module.css';
import b from './Other.module.css';
export const Button = () => (
  <button className={\`p-4 m-4 \${a.a} \${b.b}\`}>Save</button>
);
`,
  });
  await writeFile(join(cwd, "Other.module.css"), ".b { margin: 7px; }\n");

  const report = await migrate({ cwd });
  const warnings = report.warnings.filter((entry) => entry.code === "existing-tailwind-conflict");

  assert.equal(warnings.length, 2);
  assert.equal(new Set(warnings.map((warning) => `${warning.start}:${warning.end}`)).size, 1);
  assert.equal(new Set(warnings.map((warning) => warning.message)).size, 2);
});

test("validates API-only migration options", async () => {
  const cwd = await fixture();
  await assert.rejects(
    migrate({ cwd, styleFile: "Button.module.css", workspaces: true }),
    /styleFile cannot be combined with workspaces/,
  );
  await assert.rejects(
    migrate({ cwd, styleFile: "legacy.pcss" }),
    /Only \.css, \.scss, \.sass, \.less, and \.vue files can be migrated/,
  );
  await assert.rejects(
    migrate({ cwd, tailwindCss: "globals.scss" }),
    /Tailwind CSS entry must be a \.css file/,
  );
});

test("normalizes separators when resolving source map roots", () => {
  const sourceRoot = pathToFileURL(`${join(tmpdir(), "nested")}/`).href;
  assert.equal(
    sourceMappings({
      version: 3,
      sourceRoot,
      sources: ["input.scss"],
      names: [],
      mappings: "AAAA",
    })[0].sourcePath,
    join(tmpdir(), "nested", "input.scss"),
  );
  assert.equal(
    sourceMappings({
      version: 3,
      sourceRoot,
      sources: ["../input.scss"],
      names: [],
      mappings: "AAAA",
    })[0].sourcePath,
    join(tmpdir(), "input.scss"),
  );
  assert.equal(
    sourceMappings({
      version: 3,
      sourceRoot: `${sourceRoot}/`,
      sources: ["../input.scss"],
      names: [],
      mappings: "AAAA",
    })[0].sourcePath,
    join(tmpdir(), "input.scss"),
  );
});

test("retains nested SCSS rules whose expansion prevents a unique authored mapping", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    writeFile(
      join(cwd, "Card.module.scss"),
      ".parent { padding: 13px; .child { margin: 12px; } }\n",
    ),
    writeFile(
      join(cwd, "Card.tsx"),
      "import styles from './Card.module.scss';\nexport const Card = () => <div className={styles.parent}><span className={styles.child} /></div>;\n",
    ),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.module.scss" });
  assert.deepEqual(
    report.warnings.map((warning) => [
      warning.code,
      warning.start,
      warning.end,
      warning.line,
      warning.column,
      warning.endLine,
      warning.endColumn,
    ]),
    [["unproven-source-map", 0, 0, undefined, undefined, undefined, undefined]],
  );
});

test("retains a disproven SCSS descendant relationship with authored offsets", async () => {
  const source = "$m: 12px;\n.parent { padding: 13px; }\n.parent .child { margin: $m; }\n";
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    writeFile(join(cwd, "Card.module.scss"), source),
    writeFile(
      join(cwd, "Card.tsx"),
      "import styles from './Card.module.scss';\nexport const Card = () => <><div className={styles.parent} /><span className={styles.child} /></>;\n",
    ),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.module.scss" });
  const warning = report.warnings.find(
    (entry) => entry.code === "unproven-css-module-relationship",
  )!;
  const start = source.indexOf(".parent .child");
  assert.deepEqual(
    [warning.file, warning.start, warning.end],
    ["Card.module.scss", start, source.indexOf("}", start) + 1],
  );
});

test("only follows real top-level CSS imports and preserves media warning offsets", async () => {
  const cwd = await fixture();
  const source =
    '/* 한글 */\n@import "./print.css" print;\n@import "./speech.css" speech;\n.fake::before { content: "@import \'./trap.css\';"; }\n';
  await Promise.all([
    writeFile(join(cwd, "base.css"), source),
    writeFile(join(cwd, "print.css"), ".print { padding: 13px; }\n"),
    writeFile(join(cwd, "speech.css"), ".speech { height: 100vh; }\n"),
    writeFile(
      join(cwd, "index.html"),
      '<link rel="stylesheet" href="./base.css"><div class="print speech"></div>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  const warning = report.warnings.find(
    (entry) => entry.code === "unsupported-link-media" && entry.file === "base.css",
  )!;
  assert.equal(
    warning.start,
    Buffer.byteLength(source.slice(0, source.indexOf('@import "./speech.css"'))),
  );
});

test("anchors Sass compile-failure warnings to authored offsets", async () => {
  const source = "$space: 13px;\n.pad { padding: $space; }\n.button { COLOR: red; }\n";
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    writeFile(join(cwd, "Button.module.scss"), source),
    writeFile(
      join(cwd, "Button.tsx"),
      "import styles from './Button.module.scss';\nexport const Button = () => <button className={styles.button}><i className={styles.pad} /></button>;\n",
    ),
  ]);
  const report = await migrate({ cwd, styleFile: "Button.module.scss" });
  const warning = report.warnings.find((entry) => entry.code === "candidate-compilation-failure")!;
  const start = source.indexOf(".button");
  const end = source.indexOf("}", start) + 1;
  assert.equal(warning.file, "Button.module.scss");
  assert.ok(warning.start >= start && warning.end <= end && warning.end > warning.start);
});

test("canonicalizes literal utilities to the target design system's names", async () => {
  const cwd = await fixture({
    css: ".button { margin-right: auto; max-width: 100%; padding: 13px; }\n",
  });
  const report = await migrate({ cwd, styleFile: "Button.module.css", write: true });
  assert.deepEqual(report.candidates, ["max-w-full", "mr-auto", "p-[13px]"]);
  assert.match(await readFile(join(cwd, "Button.tsx"), "utf8"), /max-w-full mr-auto p-\[13px\]/);
});

test("Vue template class prefixes reserve canonical spellings", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "Card.vue"),
    "<template>\n  <div :data-x=\"`mr-${side}`\">Card</div>\n</template>\n<script setup>\nconst side = 'auto';\n</script>\n",
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("undiscovered stylesheet imports keep group spellings arbitrary", async () => {
  const cwd = await fixture({
    css: ".button { margin-right: auto; }\n",
    tsx: "import styles from './Button.module.css';\nimport 'legacy-package/styles.css';\nexport const Button = () => <button className={styles.button}>B</button>;\n",
  });
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("inline HTML script prefixes reserve canonical spellings", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "index.html"),
    "<script>document.body.className = `mr-${'auto'}`;</script><div class=\"card\"></div>\n",
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("unresolved stylesheet imports inside retained sheets keep spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "legacy.css"),
    '@import "legacy-package/theme.css";\n.legacy { color: red; }\n',
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("opaque Vue scripts keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <div class="card">Card</div>\n</template>\n<script lang="coffee">\nload "./legacy.css"\n</script>\n',
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("scanner-ignored sources keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await Promise.all([
    writeFile(join(cwd, ".gitignore"), "Ignored.tsx\n.tmp\n"),
    writeFile(join(cwd, "Ignored.tsx"), 'export const Note = () => <i className="mr-auto" />;\n'),
  ]);
  execFileSync("git", ["init", "-q"], { cwd });
  const report = await migrate({ cwd, workspaces: true });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("source-imported workspace entries keep group spellings arbitrary", async () => {
  const cwd = await fixture({
    css: ".button { margin-right: auto; }\n",
    tsx: "import styles from './Button.module.css';\nimport './other.css';\nexport const Button = () => <button className={styles.button}>B</button>;\n",
  });
  await writeFile(join(cwd, "other.css"), '@import "tailwindcss";\n');
  const report = await migrate({ cwd, styleFile: "Button.module.css", tailwindCss: "globals.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("Vue style block selectors reserve canonical spellings", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  // A scoped `.mr-auto` still styles a migrated element inside its own
  // SFC. The full-package run analyzes the SFC, so the reservation comes
  // from the block scan rather than blanket opacity.
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <div class="mr-auto">Card</div>\n</template>\n<style scoped>\n.mr-auto { appearance: none; }\n</style>\n',
  );
  const report = await migrate({ cwd });
  assert.ok(report.candidates.includes("mr-[auto]"), report.candidates.join(" "));
  assert.ok(!report.candidates.includes("mr-auto"), report.candidates.join(" "));
});

test("unresolved Vue block imports keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <div class="card">Card</div>\n</template>\n<style scoped>\n@import "legacy-package/theme.css";\n.card { appearance: none; }\n</style>\n',
  );
  const report = await migrate({ cwd });
  assert.ok(report.candidates.includes("mr-[auto]"), report.candidates.join(" "));
  assert.ok(!report.candidates.includes("mr-auto"), report.candidates.join(" "));
});

test("unresolved Vue style sources keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <div class="card">Card</div>\n</template>\n<style scoped src="@/legacy.css"></style>\n',
  );
  const report = await migrate({ cwd });
  assert.ok(report.candidates.includes("mr-[auto]"), report.candidates.join(" "));
  assert.ok(!report.candidates.includes("mr-auto"), report.candidates.join(" "));
});

test("non-colliding Vue style blocks keep canonicalization enabled", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <div class="card">Card</div>\n</template>\n<style scoped>\n.card { appearance: none; }\n</style>\n',
  );
  const report = await migrate({ cwd });
  assert.ok(report.candidates.includes("mr-auto"), report.candidates.join(" "));
});

test("unanalyzed Vue files keep scoped-run spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  // A styleFile-scoped run never prepares the SFC, so its blocks and
  // dynamic classes are unknowable and the group stays arbitrary.
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <div class="card">Card</div>\n</template>\n<style scoped>\n.card { appearance: none; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("globally composed module classes reserve canonical spellings", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "Legacy.module.css"),
    ".featured { composes: mr-auto from global; color: red; }\n",
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("authored selectors reserve canonical spellings", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(join(cwd, "legacy.css"), ".mr-auto { color: red; }\n");
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("resolved local stylesheet links keep canonicalization enabled", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await Promise.all([
    writeFile(
      join(cwd, "index.html"),
      '<link rel="stylesheet" href="./Button.module.css"><div class="button"></div><main id="hero">T</main>\n',
    ),
    writeFile(join(cwd, "unlinked.html"), '<div class="button"></div>\n'),
  ]);
  const report = await migrate({ cwd, styleFile: "Button.module.css", tailwindCss: "globals.css" });
  assert.deepEqual(report.candidates, ["mr-auto"]);
});

test("unresolved local stylesheet links keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "index.html"),
    '<link rel="stylesheet" href="/vendor/bootstrap.css"><div class="card"></div>\n',
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("constrained dynamic class templates reserve canonical spellings", async () => {
  const cwd = await fixture({
    css: ".button { margin-right: auto; }\n",
    tsx: "import styles from './Button.module.css';\nexport const Button = ({ side }) => <button className={styles.button} data-x={`mr-${side}`}>B</button>;\n",
  });
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("constrained dynamic class concatenations reserve canonical spellings", async () => {
  const cwd = await fixture({
    css: ".button { margin-right: auto; }\n",
    tsx: "import styles from './Button.module.css';\nexport const Button = ({ side }) => <button className={styles.button} data-x={'mr-' + side}>B</button>;\n",
  });
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("documents with a base element keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "index.html"),
    '<base href="https://cdn.example/"><link rel="stylesheet" href="./Button.module.css"><div class="card"></div>\n',
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("scanner-ignored sources stay reserved without workspaces mode", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await Promise.all([
    writeFile(join(cwd, ".gitignore"), "Ignored.tsx\n"),
    writeFile(join(cwd, "Ignored.tsx"), 'export const Note = () => <i className="mr-auto" />;\n'),
  ]);
  execFileSync("git", ["init", "-q"], { cwd });
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("entries excluding candidates keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "globals.css"),
    '@import "tailwindcss";\n@source not inline("mr-auto");\n',
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("entries disabling automatic scanning keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(join(cwd, "globals.css"), '@import "tailwindcss" source(none);\n');
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("selector custom variants reserve their referenced classes", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "globals.css"),
    '@import "tailwindcss";\n@custom-variant parent-auto (&:where(.mr-auto *));\n',
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("scss Vue blocks report their undiscovered dependencies", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  // The css parser would accept the @use at-rule without recording the
  // dependency, so the declared block language must drive parsing. The
  // target lives where discovery never scans, yet Sass still loads it.
  await mkdir(join(cwd, "node_modules", "legacy"), { recursive: true });
  await writeFile(
    join(cwd, "node_modules", "legacy", "_theme.scss"),
    ".mr-legacy { color: red; }\n",
  );
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <div class="card">Card</div>\n</template>\n<style scoped lang="scss">\n@use "./node_modules/legacy/theme";\n.card { appearance: none; }\n</style>\n',
  );
  const report = await migrate({ cwd });
  assert.ok(report.candidates.includes("mr-[auto]"), report.candidates.join(" "));
  assert.ok(!report.candidates.includes("mr-auto"), report.candidates.join(" "));
});

test("entries loading plugins keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await Promise.all([
    writeFile(join(cwd, "noop-plugin.js"), "module.exports = () => {};\n"),
    writeFile(join(cwd, "globals.css"), '@import "tailwindcss";\n@plugin "./noop-plugin.js";\n'),
  ]);
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("JSX stylesheet links to remote sheets keep group spellings arbitrary", async () => {
  const cwd = await fixture({
    css: ".button { margin-right: auto; }\n",
    tsx: 'import styles from \'./Button.module.css\';\nexport const Button = () => (<head><link rel="stylesheet" href="https://cdn.example/legacy.css" /><button className={styles.button}>B</button></head>);\n',
  });
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("JSX links to snapshot stylesheets keep canonicalization enabled", async () => {
  const cwd = await fixture({
    css: ".button { margin-right: auto; }\n",
    tsx: 'import styles from \'./Button.module.css\';\nexport const Button = () => (<head><link rel="stylesheet" href="./theme.css" /><button className={styles.button}>B</button></head>);\n',
  });
  await writeFile(join(cwd, "theme.css"), ".card { color: red; }\n");
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-auto"]);
});

test("templated HTML class attributes keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(join(cwd, "index.html"), '<div class="{{ cls }}">T</div>\n');
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("prefix-constrained templated HTML classes reserve their prefixes", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  // The btn- prefix cannot complete to mr-auto, so canonicalization
  // stays enabled while the template's own family is reserved.
  await writeFile(join(cwd, "index.html"), '<div class="card btn-{{ kind }}">T</div>\n');
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-auto"]);
});

test("opaque page style sources keep group spellings arbitrary", async () => {
  const cwd = await fixture({ css: ".button { margin-right: auto; }\n" });
  await writeFile(
    join(cwd, "index.html"),
    '<link rel="stylesheet" href="https://cdn.example.com/legacy.css"><div class="card"></div>\n',
  );
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  assert.deepEqual(report.candidates, ["mr-[auto]"]);
});

test("escapes literal underscores in arbitrary values", async () => {
  const cwd = await fixture({ css: ".button { --font-key: Open_Sans; }\n" });
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  const designSystem = await loadDesignSystem("@tailwind utilities;");
  const css = designSystem.candidatesToCss(report.candidates).join("");
  assert.match(css, /Open_Sans/);
  assert.doesNotMatch(css, /Open Sans/);
});

test("round-trips quoted values and urls through arbitrary candidates", async () => {
  const cwd = await fixture({
    css: '.button { background-image: url("a_b.png"); font-family: "My Font", sans-serif; content: "a_b"; width: calc(min(100%, 50vw)); }\n',
  });
  const report = await migrate({ cwd, styleFile: "Button.module.css" });
  const designSystem = await loadDesignSystem("@tailwind utilities;");
  const css = designSystem.candidatesToCss(report.candidates).join("");
  assert.match(css, /url\("a_b\.png"\)/);
  assert.match(css, /"My Font", sans-serif/);
  assert.match(css, /content: "a_b"/);
  assert.match(css, /calc\(min\(100%, 50vw\)\)/);
});

test("migrates a closed Vue SFC byte-exactly and removes the emptied scoped block", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">한글</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.deepEqual(
    await readFile(join(cwd, "Card.vue")),
    Buffer.from(
      '<template>\n  <p class="card p-[13px]">한글</p>\n  <p class="etc">B</p>\n</template>\n',
    ),
  );
  const second = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(second.changedFiles, []);
});

test("retains a single-root Vue SFC scoped rule while appending its utilities", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <div class="panel">A</div>\n</template>\n<style scoped>\n.panel { margin: 7px; }\n</style>\n';
  await writeFile(join(cwd, "Panel.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Panel.vue" });
  const warning = report.warnings.find((entry) => entry.code === "open-root-fallthrough")!;
  const ruleStart = Buffer.byteLength(vue.slice(0, vue.indexOf(".panel {")));
  assert.deepEqual(
    [warning.file, warning.start, warning.end],
    ["Panel.vue", ruleStart, ruleStart + ".panel { margin: 7px; }".length],
  );
  assert.match(report.diff, /class="panel m-\[7px\]"/);
  assert.match(report.diff, /\+<style scoped>/);
  assert.equal(report.retainedRules, 1);
  assert.deepEqual(report.changedFiles, ["Panel.vue"]);
});

test("migrates supported preprocessors beside retained Vue style blocks", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped lang="scss">\n.card { padding: 13px; }\n</style>\n<style>\n.free { color: red; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(report.changedFiles, ["Card.vue"]);
  assert.deepEqual(
    report.warnings.map((entry) => entry.code),
    ["unscoped-style-block"],
  );
  assert.match(report.diff, /class="card p-\[13px\]"/);
});

test("locates scoped blocks with whitespace in their closing tags", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="note">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style >\n<style scoped>\n.note { margin: 3px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.equal(
    await readFile(join(cwd, "Card.vue"), "utf8"),
    '<template>\n  <p class="card p-[13px]">A</p>\n  <p class="note m-[3px]">B</p>\n</template>\n',
  );
});

test("reports a Vue parse error at the first byte", async () => {
  const cwd = await fixture();
  await writeFile(join(cwd, "Broken.vue"), "<template>");

  const report = await migrate({ cwd, styleFile: "Broken.vue" });
  const warning = report.warnings.find((entry) => entry.code === "unsupported-sfc-block")!;

  assert.deepEqual(
    [warning.start, warning.end, warning.line, warning.column, warning.endLine, warning.endColumn],
    [0, 0, 1, 1, 1, 1],
  );
});

test("retains an SFC with a custom block", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A\u2028B</p>\n  <p class="note">B</p>\n</template>\n<docs>runtime transform</docs>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  const warning = report.warnings.find((entry) => entry.code === "unsupported-sfc-block")!;
  assert.deepEqual(report.changedFiles, []);
  assert.equal(warning.line, 5);
});

test("retains scoped styles with unsupported behavioral attributes", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="note">B</p>\n</template>\n<style scoped media="print">\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(report.changedFiles, []);
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-sfc-block"));
});

test("treats a dynamic v-bind argument as an open class surface", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p v-bind:[key]="v" class="etc">B</p>\n</template>\n<script setup>\nconst key = "class";\nconst v = "x";\n</script>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  const warning = report.warnings.find((entry) => entry.code === "dynamic-template-class")!;
  assert.equal(warning.file, "Card.vue");
  assert.equal(report.convertedRules, 0);
  assert.equal(report.retainedRules, 1);
  assert.match(report.diff, /\+<style scoped>/);
});

test("migrates static Vue class bindings on hosts and component calls", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      "<template>\n  <div>Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n",
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Child :class="\'passed\'" />\n  <main :class="`local`">Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n<style scoped>\n.local { margin: 7px; }\n</style>\n',
    ),
  ]);
  const child = await migrate({ cwd, styleFile: "Child.vue", write: true });
  const parent = await migrate({ cwd, styleFile: "Parent.vue", write: true });
  const output = await readFile(join(cwd, "Parent.vue"), "utf8");
  assert.equal(child.convertedRules, 1);
  assert.equal(parent.convertedRules, 1);
  assert.match(output, /<Child :class="'passed'" class="p-\[13px\]" \/>/);
  assert.match(output, /<main :class="`local`" class="m-\[7px\]">/);
  assert.doesNotMatch(await readFile(join(cwd, "Child.vue"), "utf8"), /<style/);
  assert.doesNotMatch(output, /<style/);
});

test("keeps non-static Vue class bindings opaque", async () => {
  for (const binding of [
    "{ card: active }",
    "['card']",
    "active ? 'card' : 'other'",
    "active && 'card'",
    "`card-${size}`",
    "'card' + suffix",
    "classes",
    "pick()",
  ]) {
    const cwd = await fixture();
    const vue = `<template>\n  <p :class="${binding}">Bound</p>\n  <p>Sibling</p>\n</template>\n<script setup>\nconst active = true;\nconst size = 1;\nconst suffix = "";\nconst classes = "card";\nconst pick = () => "card";\n</script>\n<style scoped>\n.card { padding: 13px; }\n</style>\n`;
    await writeFile(join(cwd, "Card.vue"), vue);
    const report = await migrate({ cwd, styleFile: "Card.vue" });
    assert.deepEqual(report.changedFiles, [], binding);
    assert.equal(report.convertedRules, 0, binding);
    assert.equal(report.retainedRules, 1, binding);
    assert.ok(
      report.warnings.some((entry) => entry.code === "dynamic-template-class"),
      binding,
    );
  }
});

test("uses DOM ASCII whitespace for static Vue class bindings", async () => {
  const cwd = await fixture();
  const vue =
    "<template>\n  <p :class=\"'a b'\">Bound</p>\n  <p>Sibling</p>\n</template>\n<style scoped>\n.a { padding: 13px; }\n</style>\n";
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(report.changedFiles, []);
  assert.equal(report.convertedRules, 0);
  assert.equal(report.retainedRules, 1);
});

test("does not synthesize classes beside unsafe Vue attributes", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="a&amp;b" :class="\'card\'">Bound</p>\n  <p>Sibling</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(report.changedFiles, []);
  assert.equal(report.convertedRules, 0);
  assert.equal(report.retainedRules, 1);
  assert.ok(report.warnings.some((entry) => entry.code === "dynamic-template-class"));
});

test("retains a scoped rule whose class other package CSS also targets", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="only">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n.only { margin: 3px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "site.css"), ".card { padding: 20px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  const warning = report.warnings.find((entry) => entry.code === "shadowed-scoped-rule")!;
  assert.equal(warning.file, "Card.vue");
  // The unshadowed rule in the same block still migrates.
  assert.equal(report.convertedRules, 1);
  assert.equal(report.retainedRules, 1);
  assert.match(report.diff, /class="only m-\[3px\]"/);
  assert.match(report.diff, /\+\.card \{ padding: 13px; \}/);
});

test("appends utilities to a literal class site beside a dynamic binding", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="row" :class="tone">A</p>\n</template>\n<script setup>\nconst tone = "warm";\n</script>\n<style scoped>\n.row { margin: 7px; }\n</style>\n';
  await writeFile(join(cwd, "Row.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Row.vue" });
  assert.match(report.diff, /class="row m-\[7px\]" :class="tone"/);
  assert.equal(report.retainedRules, 1);
  assert.ok(report.warnings.some((entry) => entry.code === "dynamic-template-class"));
});

test("ignores custom directive runtime mutations outside static template scope", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p v-highlight class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.equal(report.convertedRules, 1);
  assert.equal(report.retainedRules, 0);
  assert.match(report.diff, /class="card p-\[13px\]"/);
});

test("a :global escape in a module stylesheet shadows scoped deletion", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "Theme.module.css"), ":global(.card) { padding: 20px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("a gitignored SFC's unscoped block shadows scoped deletion", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  const legacy =
    "<template>\n  <p>L</p>\n  <p>M</p>\n</template>\n<style>\n.card { padding: 20px; }\n</style>\n";
  execFileSync("git", ["init", "-q"], { cwd });
  await Promise.all([
    writeFile(join(cwd, ".gitignore"), "Legacy.vue\n.tmp\n"),
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "Legacy.vue"), legacy),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("withholds quote-bearing candidates inside Vue class attributes", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { font-family: "My Font", sans-serif; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(report.changedFiles, []);
  assert.equal(report.retainedRules, 1);
  assert.ok(report.warnings.some((entry) => entry.code === "unresolved-selector-target"));
});

test("reports Vue-only warnings without requiring a Tailwind entry", async () => {
  const cwd = await tempDir();
  const vue =
    '<template>\n  <p class="note">A</p>\n</template>\n<style>\n.note { margin: 3px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "package.json"), '{"private":true}'),
    writeFile(join(cwd, "site.css"), ".plain { color: red; }\n"),
    writeFile(join(cwd, "Blocks.vue"), vue),
  ]);
  const report = await migrate({ cwd, styleFile: "Blocks.vue" });
  assert.deepEqual(report.changedFiles, []);
  assert.deepEqual(
    report.warnings.map((entry) => entry.code),
    ["unscoped-style-block"],
  );
});

test("interpolated preprocessor selectors make the shadow corpus unverifiable", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "theme.scss"), "$name: card;\n.#{$name} { padding: 20px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("unparseable scan-only stylesheets make the Vue shadow corpus unverifiable", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "broken.css"), "}"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
  assert.deepEqual(report.failures, []);
});

test("warns when a Vue element already carries a conflicting utility", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card p-4">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  const warning = report.warnings.find((entry) => entry.code === "existing-tailwind-conflict")!;
  assert.equal(warning.file, "Card.vue");
  // Parity with the JS path: the conversion still happens.
  assert.equal(report.convertedRules, 1);
  assert.match(report.diff, /class="card p-4 p-\[13px\]"/);
});

test("keeps rebased Vue conflict warnings on authored offsets", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <div class="card p-4">Card</div>\n</template>\n<script setup>\nimport "./a.css";\nimport "./b.css";\n</script>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "a.css"), ".card { margin: 7px; }\n"),
    writeFile(join(cwd, "b.css"), ".card { padding: 13px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  const warning = report.warnings.find((entry) => entry.code === "existing-tailwind-conflict")!;
  const start = Buffer.byteLength(vue.slice(0, vue.indexOf("card p-4")));
  assert.deepEqual([warning.start, warning.end], [start, start + Buffer.byteLength("card p-4")]);
});

test("a :deep escape in another SFC shadows scoped deletion", async () => {
  const cwd = await fixture();
  const child =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  const parent =
    '<template>\n  <div class="wrap">P</div>\n</template>\n<style scoped>\n.wrap :deep(.card) { padding: 20px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Child.vue"), child),
    writeFile(join(cwd, "Parent.vue"), parent),
  ]);
  const report = await migrate({ cwd, styleFile: "Child.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("a deep escape nested in a functional pseudo shadows scoped deletion", async () => {
  const cwd = await fixture();
  const child =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  const parent =
    '<template>\n  <div class="wrap">P</div>\n</template>\n<style scoped>\n.wrap :deep(.host) :is(:deep(.card)) { padding: 20px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Child.vue"), child),
    writeFile(join(cwd, "Parent.vue"), parent),
  ]);
  const report = await migrate({ cwd, styleFile: "Child.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("a parsed nested deep escape does not shadow unrelated scoped rules", async () => {
  const cwd = await fixture();
  const child =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  const parent =
    '<template>\n  <div class="wrap">P</div>\n</template>\n<style scoped>\n.wrap :deep(.other:is(.x, .y)) { padding: 20px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Child.vue"), child),
    writeFile(join(cwd, "Parent.vue"), parent),
  ]);
  const report = await migrate({ cwd, styleFile: "Child.vue", write: true });
  assert.equal(report.convertedRules, 1);
  assert.doesNotMatch(await readFile(join(cwd, "Child.vue"), "utf8"), /<style/);
});

test("a paren-less deep combinator makes the shadow corpus unverifiable", async () => {
  const cwd = await fixture();
  const child =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  const parent =
    '<template>\n  <div class="wrap">P</div>\n</template>\n<style scoped>\n.wrap ::v-deep .card { padding: 20px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Child.vue"), child),
    writeFile(join(cwd, "Parent.vue"), parent),
  ]);
  const report = await migrate({ cwd, styleFile: "Child.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("migrates a stylesheet statically imported by a Vue script", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<script setup lang="ts">\nimport type { Props } from "./props";\ndefineProps<Props>();\nimport "./card.css";\n</script>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "card.css"), ".card { padding: 13px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.deepEqual(report.changedFiles, ["Card.vue"]);
  assert.equal(report.convertedRules, 0);
  assert.equal(report.retainedRules, 1);
  assert.match(report.diff, /class="card p-\[13px\]"/);
  assert.match(report.diff, /import "\.\/card\.css";/);
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /class="card p-\[13px\]"/);
  const second = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.deepEqual(second.changedFiles, []);
});

test("does not infer stylesheets from extensionless Vue script imports", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n</template>\n<script setup>\nimport helper from "./theme";\nvoid helper;\n</script>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "theme.js"), "export default 1;\n"),
    writeFile(join(cwd, "theme.css"), ".card { margin: 7px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.doesNotMatch(report.diff, /m-\[7px\]/);
});

test("propagates Vue consumer contexts through local CSS imports", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="nested">A</p>\n</template>\n<script setup>\nimport "./entry.css";\n</script>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "entry.css"), '@import "./nested.css";\n'),
    writeFile(join(cwd, "nested.css"), ".nested { margin: 7px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.match(report.diff, /class="nested m-\[7px\]"/);
});

test("routes a Vue script's imported preprocessor through the existing compiler", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<script>\nimport "./card.scss";\nexport default {};\n</script>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "card.scss"), "$space: 13px;\n.card { padding: $space; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(report.changedFiles, ["Card.vue"]);
  assert.match(report.diff, /class="card p-\[13px\]"/);
});

test("ignores dynamic and commented Vue stylesheet imports", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<script setup>\n// import "./card.css";\nimport("./card.css");\n</script>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "card.css"), ".card { padding: 13px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(report.changedFiles, []);
  assert.equal(report.convertedRules, 0);
  assert.equal(report.retainedRules, 0);
});

test("keeps imported stylesheet candidates isolated per Vue element", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "One.vue"),
      '<template>\n  <p class="shared">One</p>\n</template>\n<script setup>\nimport "./one.css";\n</script>\n',
    ),
    writeFile(
      join(cwd, "Two.vue"),
      '<template>\n  <p class="shared">Two</p>\n</template>\n<script setup>\nimport "./two.css";\n</script>\n',
    ),
    writeFile(join(cwd, "one.css"), ".shared { padding: 13px; }\n"),
    writeFile(join(cwd, "two.css"), ".shared { margin: 7px; }\n"),
  ]);
  await migrate({ cwd, write: true });
  assert.match(await readFile(join(cwd, "One.vue"), "utf8"), /class="shared p-\[13px\]"/);
  assert.doesNotMatch(await readFile(join(cwd, "One.vue"), "utf8"), /m-\[7px\]/);
  assert.match(await readFile(join(cwd, "Two.vue"), "utf8"), /class="shared m-\[7px\]"/);
  assert.doesNotMatch(await readFile(join(cwd, "Two.vue"), "utf8"), /p-\[13px\]/);
});

test("follows external Vue style blocks as stylesheet consumer edges", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped src="./card.css"></style>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "card.css"), ".card { padding: 13px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(report.changedFiles, ["Card.vue"]);
  assert.match(report.diff, /class="card p-\[13px\]"/);
  assert.ok(!report.warnings.some((entry) => entry.code === "unsupported-sfc-block"));
});

test("unresolved external Vue styles retain scoped deletions", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped src="@/theme.css"></style>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-sfc-block"));
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("does not require Tailwind for an explicitly selected Vue module import", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<script setup>\nimport styles from "./card.module.css";\n</script>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "card.module.css"), ".card { padding: 13px; }\n"),
    rm(join(cwd, "globals.css")),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(report.changedFiles, []);
});

test("closes Vue root fallthrough through static component callers", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child class="passed" />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  assert.deepEqual(
    report.warnings.filter((entry) => entry.code === "open-root-fallthrough"),
    [],
  );
  assert.match(await readFile(join(cwd, "Child.vue"), "utf8"), /class="passed p-\[13px\]"/);
  assert.match(await readFile(join(cwd, "App.vue"), "utf8"), /class="passed p-\[13px\]"/);
  assert.doesNotMatch(await readFile(join(cwd, "Child.vue"), "utf8"), /<style/);
});

test("a plain HTML page does not open Vue caller fallthrough", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child class="passed" />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(join(cwd, "index.html"), '<div id="app"></div>\n'),
  ]);
  const report = await migrate({ cwd, write: true });
  assert.ok(!report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.doesNotMatch(await readFile(join(cwd, "Child.vue"), "utf8"), /<style/);
});

test("shadowed Vue macros do not open proven root fallthrough", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<script setup>\nconst defineProps = () => {};\nconst note = "defineOptions inheritAttrs";\ndefineProps();\n</script>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child class="passed" />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  assert.ok(!report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.doesNotMatch(await readFile(join(cwd, "Child.vue"), "utf8"), /<style/);
});

test("dynamic Vue import globs keep caller fallthrough open", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(
      join(cwd, "registry.ts"),
      'export const components = import.meta.glob("./*.{vue,js}");\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.ok(
    report.rules.some(
      (rule) =>
        rule.file === "Child.vue" && rule.selector === ".passed" && rule.status === "retained",
    ),
  );
});

test("exact Vue import globs keep caller fallthrough open", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(join(cwd, "registry.ts"), 'import.meta.glob("./Child.vue");\n'),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
});

test("unreadable Vue import globs keep caller fallthrough open", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(join(cwd, "registry.ts"), 'const pattern = "./*.vue"; import.meta.glob(pattern);\n'),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
});

test("comments and strings that resemble Vue loaders do not open caller fallthrough", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child class="passed" />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(
      join(cwd, "registry.ts"),
      '// import("./Comment.vue");\nexport const note = \'import.meta.glob("./*.vue")\';\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  assert.ok(!report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.doesNotMatch(await readFile(join(cwd, "Child.vue"), "utf8"), /<style/);
});

test("extensionless local globs keep caller fallthrough open", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(
      join(cwd, "registry.ts"),
      'export const components = import.meta.glob("./components/*");\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.ok(
    report.rules.some(
      (rule) =>
        rule.file === "Child.vue" && rule.selector === ".passed" && rule.status === "retained",
    ),
  );
});

test("a self-recursive component migrates without overlapping edits", async () => {
  const cwd = await fixture();
  const tree =
    '<template>\n  <div class="node">\n    <Tree class="node" />\n  </div>\n</template>\n<script setup>\nimport Tree from "./Tree.vue";\n</script>\n<style scoped>\n.node { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Tree.vue"), tree);
  const report = await migrate({ cwd });
  // One call site, one appended utility -- never overlapping edits.
  const appended = report.diff.match(/node p-\[13px\]/g) ?? [];
  assert.ok(appended.length >= 1);
  assert.ok(!report.failures.length);
});

test("a dependency vanishing during Vue block compilation stays fatal under force", async () => {
  const cwd = await fixture();
  const fakeSass = [
    'import { pathToFileURL } from "node:url";',
    "export const compileStringAsync = async () => ({",
    '  css: ".card { padding: 13px; }",',
    "  sourceMap: {",
    "    version: 3,",
    '    sources: [pathToFileURL("gone.scss").href],',
    '    mappings: "AAAA",',
    "    names: [],",
    "  },",
    '  loadedUrls: [new URL("../../gone.scss", import.meta.url)],',
    "});",
  ].join("\n");
  await Promise.all([
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped lang="scss">\n.card { padding: 13px; }\n</style>\n',
    ),
    mkdir(join(cwd, "node_modules", "sass"), { recursive: true }),
  ]);
  await Promise.all([
    writeFile(
      join(cwd, "node_modules", "sass", "package.json"),
      '{"name":"sass","version":"1.0.0","type":"module","main":"index.js"}',
    ),
    writeFile(join(cwd, "node_modules", "sass", "index.js"), fakeSass),
  ]);
  await assert.rejects(
    migrate({ cwd, force: true }),
    /Source changed during planning:.*gone\.scss/,
  );
});

test("unresolved Vue aliases keep every possible caller surface open", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Direct.vue"),
      '<template>\n  <Child class="passed" />\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(join(cwd, "aliased.ts"), 'import Child from "@/Child.vue";\nvoid Child;\n'),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.ok(
    report.rules.some(
      (rule) =>
        rule.file === "Child.vue" && rule.selector === ".passed" && rule.status === "retained",
    ),
  );
});

test("dynamic aliased Vue imports keep caller fallthrough open", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Direct.vue"),
      '<template>\n  <Child class="passed" />\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(
      join(cwd, "dynamic.ts"),
      'void import(/* webpackChunkName: "child" */ "components/Child");\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.ok(
    report.rules.some(
      (rule) =>
        rule.file === "Child.vue" && rule.selector === ".passed" && rule.status === "retained",
    ),
  );
});

test("dynamic aliased imports in Vue scripts keep caller fallthrough open", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Direct.vue"),
      '<template>\n  <Child class="passed" />\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(
      join(cwd, "Dynamic.vue"),
      '<template>\n  <main>Dynamic</main>\n</template>\n<script setup>\nvoid import("@/Child");\n</script>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.ok(
    report.rules.some(
      (rule) =>
        rule.file === "Child.vue" && rule.selector === ".passed" && rule.status === "retained",
    ),
  );
});

test("foreign workspace Vue sources keep caller fallthrough open", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    rm(join(cwd, "globals.css")),
    writeFile(join(cwd, "package.json"), '{"private":true,"workspaces":["packages/*"]}'),
    mkdir(join(cwd, "packages", "lib"), { recursive: true }),
    mkdir(join(cwd, "packages", "app"), { recursive: true }),
  ]);
  await Promise.all([
    writeFile(join(cwd, "packages", "lib", "package.json"), '{"private":true}'),
    writeFile(join(cwd, "packages", "lib", "globals.css"), '@import "tailwindcss";\n'),
    writeFile(
      join(cwd, "packages", "lib", "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "packages", "lib", "Parent.vue"),
      '<template>\n  <Child class="passed" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(join(cwd, "packages", "app", "package.json"), '{"private":true}'),
    writeFile(
      join(cwd, "packages", "app", "App.vue"),
      '<template>\n  <main>App</main>\n</template>\n<script setup>\nvoid import("../../lib/Child.vue");\n</script>\n',
    ),
  ]);
  execFileSync("git", ["init", "-q"], { cwd });
  const report = await migrate({ cwd, workspaces: true, extractMediaQueries: false });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.ok(
    report.rules.some(
      (rule) =>
        rule.file === "packages/lib/Child.vue" &&
        rule.selector === ".passed" &&
        rule.status === "retained",
    ),
  );
});

test("does not rewrite symlinked Vue files reached by component proof", async (context) => {
  if (process.platform === "win32") context.skip("symlink creation requires elevated privileges");
  const cwd = await fixture();
  await writeFile(
    join(cwd, "RealChild.vue"),
    '<template>\n  <div class="leaf">Leaf</div>\n</template>\n',
  );
  await symlink("RealChild.vue", join(cwd, "Child.vue"));
  await writeFile(
    join(cwd, "Parent.vue"),
    '<template>\n  <Child />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n<style scoped>\n.leaf { margin: 7px; }\n</style>\n',
  );
  await migrate({ cwd, styleFile: "Parent.vue", write: true });
  assert.equal((await lstat(join(cwd, "Child.vue"))).isSymbolicLink(), true);
});

test("writes parent scoped utilities at the proven component call site", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(join(cwd, "Leaf.vue"), '<template>\n  <div class="leaf">Leaf</div>\n</template>\n'),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Leaf />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Leaf from "./Leaf.vue";\n</script>\n<style scoped>\n.leaf { margin: 7px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Other.vue"),
      '<template>\n  <Leaf />\n  <main>Other</main>\n</template>\n<script setup>\nimport Leaf from "./Leaf.vue";\n</script>\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  assert.deepEqual(
    report.warnings.filter((entry) => entry.code === "component-class-target"),
    [],
  );
  assert.match(await readFile(join(cwd, "Parent.vue"), "utf8"), /<Leaf class="m-\[7px\]" \/>/);
  assert.doesNotMatch(await readFile(join(cwd, "Parent.vue"), "utf8"), /<style/);
  assert.doesNotMatch(await readFile(join(cwd, "Leaf.vue"), "utf8"), /m-\[7px\]/);
  assert.doesNotMatch(await readFile(join(cwd, "Other.vue"), "utf8"), /m-\[7px\]/);
});

test("a component chained through a single-root parent stays open", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="item">Child</div>\n</template>\n<style scoped>\n.item { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Wrapper.vue"),
      '<template>\n  <Child />\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Wrapper class="item" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Wrapper from "./Wrapper.vue";\n</script>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  // Parent's class chains through Wrapper's component root to Child, so
  // Child's scoped rule must not be deleted.
  assert.ok(
    report.rules.some(
      (rule) =>
        rule.file === "Child.vue" && rule.selector === ".item" && rule.status === "retained",
    ),
  );
});

test("v-html defeats the unscoped sole-source proof", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <div class="shared">Card</div>\n  <span v-html="raw"></span>\n</template>\n<script setup>\nconst raw = "<i>x</i>";\n</script>\n<style>\n.shared { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "unscoped-style-block"));
  assert.equal(report.convertedRules, 0);
});

test("an unresolved package stylesheet import shadows scoped deletion", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<script setup>\nimport "bootstrap/dist/css/bootstrap.css";\n</script>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("warns when a fallthrough utility conflicts with a child-root class", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="leaf p-2">Child</div>\n</template>\n<style scoped>\n.external { padding: 1rem; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Child class="external" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  const warning = report.warnings.find((entry) => entry.code === "existing-tailwind-conflict");
  assert.ok(warning);
  assert.match(warning.message, /p-2/);
});

test("bare stylesheet specifiers never bind coincidental local files", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card other">A</p>\n  <p class="etc">B</p>\n</template>\n<script setup>\nimport "theme/card.css";\n</script>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await mkdir(join(cwd, "theme"), { recursive: true });
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "theme", "card.css"), ".other { margin: 3px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  // The bare specifier is a package import: it must not consume the local
  // decoy, and its unresolved global CSS retains the scoped rule.
  assert.ok(!report.diff.includes("m-[3px]"));
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("explicit non-Vue local imports do not open proven caller surfaces", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child class="passed" />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\nimport { helper } from "./utils.ts";\nvoid helper;\n</script>\n',
    ),
    writeFile(join(cwd, "utils.ts"), "export const helper = 1;\n"),
  ]);
  const report = await migrate({ cwd, write: true });
  assert.deepEqual(
    report.warnings.filter((entry) => entry.code === "open-root-fallthrough"),
    [],
  );
  assert.match(await readFile(join(cwd, "Child.vue"), "utf8"), /class="passed p-\[13px\]"/);
  assert.doesNotMatch(await readFile(join(cwd, "Child.vue"), "utf8"), /<style/);
});

test("a root v-for child renders a fragment and blocks call-site rewrites", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div v-for="item in items" class="leaf">{{ item }}</div>\n</template>\n<script setup>\nconst items = ["a"];\n</script>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Child class="leaf" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n<style scoped>\n.leaf { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  // The fragment child never receives the call-site class, so the parent
  // rule must be retained and the call site left alone.
  const rule = report.rules.find(
    (entry) => entry.file === "Parent.vue" && entry.selector === ".leaf",
  )!;
  assert.equal(rule.status, "retained");
  assert.doesNotMatch(await readFile(join(cwd, "Parent.vue"), "utf8"), /m-\[7px\]/);
});

test("a text root fragments the child and blocks call-site rewrites", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="leaf">A</div>\n  text tail\n</template>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Child class="leaf" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n<style scoped>\n.leaf { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  const rule = report.rules.find(
    (entry) => entry.file === "Parent.vue" && entry.selector === ".leaf",
  )!;
  assert.equal(rule.status, "retained");
  assert.doesNotMatch(await readFile(join(cwd, "Parent.vue"), "utf8"), /m-\[7px\]/);
});

test("a multi-root child never receives call-site rewrites", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="leaf">A</div>\n  <span>B</span>\n</template>\n<style scoped>\n.leaf { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Child class="leaf" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  // Vue cannot inherit attributes onto a multi-root child, so the call
  // site stays untouched while the internal usage migrates.
  assert.doesNotMatch(await readFile(join(cwd, "Parent.vue"), "utf8"), /p-\[13px\]/);
  assert.match(await readFile(join(cwd, "Child.vue"), "utf8"), /class="leaf p-\[13px\]"/);
  assert.ok(!report.failures.length);
});

test("CSS v-bind declarations retain across plain and preprocessor blocks", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { color: v-bind(theme); }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Tone.vue"),
      '<template>\n  <p class="tone">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped lang="scss">\n.tone { color: v-bind("theme.color"); }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  // Vue rewrites v-bind() only inside SFC style compilation; a migrated
  // utility would lose the reactive value.
  assert.ok(
    report.rules
      .filter((rule) => rule.file.endsWith(".vue"))
      .every((rule) => rule.status === "retained"),
  );
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-value"));
});

test("counts form feeds within Vue style warning locations", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\f.card { color: v-bind(theme); }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);

  const report = await migrate({ cwd, styleFile: "Card.vue" });
  const warning = report.warnings.find((entry) => entry.code === "unsupported-value")!;

  assert.equal(warning.line, 6);
});

test("isolates Vue form-feed line counts to the warning block", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="first second">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\f.first { padding: 13px; }\n</style>\n<style scoped>\n.second { color: v-bind(theme); }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);

  const report = await migrate({ cwd, styleFile: "Card.vue" });
  const warning = report.warnings.find((entry) => entry.code === "unsupported-value")!;

  assert.equal(warning.line, 8);
});

test("counts form feeds within retained Vue style warning ranges", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n</template>\n<style>\f.card { color: red; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);

  const report = await migrate({ cwd, styleFile: "Card.vue" });
  const warning = report.warnings.find((entry) => entry.code === "unscoped-style-block")!;

  assert.deepEqual([warning.line, warning.endLine], [4, 6]);
});

test("rewrites proven $style bindings and deletes the emptied module block", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p :class="$style.card">A</p>\n  <p class="plain" :class="$style.note">B</p>\n</template>\n<style module>\n.card { padding: 13px; }\n.note { margin: 7px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.deepEqual(
    await readFile(join(cwd, "Card.vue"), "utf8"),
    '<template>\n  <p class="p-[13px]">A</p>\n  <p class="plain m-[7px]">B</p>\n</template>\n',
  );
  const second = await migrate({ cwd, styleFile: "Card.vue" });
  assert.deepEqual(second.changedFiles, []);
});

test("preserves multiline whitespace around rewritten $style bindings", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p\n    :class="$style.card"\n  >A</p>\n  <span>B</span>\n</template>\n<style module>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.equal(
    await readFile(join(cwd, "Card.vue"), "utf8"),
    '<template>\n  <p\n    class="p-[13px]"\n  >A</p>\n  <span>B</span>\n</template>\n',
  );
});

test("shadowed Vue names and string contents do not retain a CSS Module", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p :class="$style.card">A</p>\n  <span :title="\'$style useCssModule\'" @click="useCssModule()">B</span>\n</template>\n<script setup>\nconst useCssModule = () => {};\nconst note = "$style defineProps defineOptions inheritAttrs";\nuseCssModule();\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.deepEqual(report.changedFiles, ["Card.vue"]);
  assert.doesNotMatch(await readFile(join(cwd, "Card.vue"), "utf8"), /<style module>/);
});

test("useCssModule used only from the template retains the CSS Module", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p :class="$style.card" :data-kind="useCssModule().card">Card</p>\n  <span>Leaf</span>\n</template>\n<script setup>\nimport { useCssModule } from "vue";\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /<style module>/);
});

test("Options API instance aliases through assignment retain the CSS Module", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p :class="$style.card">Card</p>\n  <span>Leaf</span>\n</template>\n<script>\nexport default { mounted() { let vm; vm = this; void vm.$style.card; } };\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /<style module>/);
});

test("TypeScript template assertions do not break the module closure", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p :class="$style.card" :title="(note as string)">Card</p>\n  <span>Leaf</span>\n</template>\n<script setup lang="ts">\nconst note: unknown = "n";\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.deepEqual(report.changedFiles, ["Card.vue"]);
  assert.doesNotMatch(await readFile(join(cwd, "Card.vue"), "utf8"), /<style module>/);
});

test("aliased useCssModule imports used only from templates retain the module", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p :class="$style.card" :data-kind="css().card">Card</p>\n  <span>Leaf</span>\n</template>\n<script setup>\nimport { useCssModule as css } from "vue";\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /<style module>/);
});

test("unreadable Vue scripts keep caller fallthrough open", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(
      join(cwd, "Loader.vue"),
      '<template>\n  <div>Loader</div>\n</template>\n<script src="./external.coffee"></script>\n',
    ),
  ]);
  const report = await migrate({ cwd, styleFile: "Child.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.equal(report.convertedRules, 0);
});

test("Vue inline event handlers use statement context for module closure", async () => {
  const cwd = await fixture();
  const component = (handler: string) =>
    `<template>\n  <p :class="$style.card" @click="${handler}">Card</p>\n  <span>Leaf</span>\n</template>\n<script setup>\nconst first = () => {};\nconst second = () => {};\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n`;
  await Promise.all([
    writeFile(join(cwd, "Clean.vue"), component("first(); second()")),
    writeFile(join(cwd, "Used.vue"), component("first(); $style.card")),
  ]);

  const clean = await migrate({ cwd, styleFile: "Clean.vue" });
  assert.equal(clean.convertedRules, 1);
  assert.ok(!clean.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));

  const used = await migrate({ cwd, styleFile: "Used.vue" });
  assert.equal(used.convertedRules, 0);
  assert.ok(used.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
});

test("destructured Options API CSS Module references retain the module", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p :class="$style.card">Card</p>\n  <span>Leaf</span>\n</template>\n<script>\nexport default { mounted() { const { $style: styles } = this; void styles.card; } };\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /<style module>/);
});

test("Options API instance aliases retain the CSS Module", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p :class="$style.card">Card</p>\n  <span>Leaf</span>\n</template>\n<script>\nexport default { mounted() { const vm = this; void vm.$style.card; } };\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /<style module>/);
});

test("TypeScript-wrapped Options API instance aliases retain the CSS Module", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p :class="$style.card">Card</p>\n  <span>Leaf</span>\n</template>\n<script lang="ts">\nexport default { mounted() { const vm = this as ComponentPublicInstance; void vm.$style.card; } };\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /<style module>/);
});

test("computed Vue namespace CSS Module references retain the module", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p :class="$style.card">Card</p>\n  <span>Leaf</span>\n</template>\n<script setup>\nimport * as Vue from "vue";\nconst styles = Vue["useCssModule"]();\nvoid styles.card;\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /<style module>/);
});

test("destructured Vue namespace helpers retain the CSS Module", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p :class="$style.card">Card</p>\n  <span>Leaf</span>\n</template>\n<script setup>\nimport * as Vue from "vue";\nconst { useCssModule } = Vue;\nvoid useCssModule().card;\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n',
  );
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /<style module>/);
});

test("$style outside proven member sites retains the module", async () => {
  const cwd = await fixture();
  const conditional =
    '<template>\n  <p :class="$style.tone">A</p>\n  <p :class="cond ? $style.tone : \'\'">B</p>\n</template>\n<script setup>\nconst cond = true;\n</script>\n<style module>\n.tone { color: red; }\n</style>\n';
  const scripted =
    '<template>\n  <p :class="$style.pad">A</p>\n</template>\n<script setup>\nimport { useCssModule } from "vue";\nconst style = useCssModule();\nvoid style;\n</script>\n<style module>\n.pad { padding: 5px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Conditional.vue"), conditional),
    writeFile(join(cwd, "Scripted.vue"), scripted),
  ]);
  const report = await migrate({ cwd });
  assert.equal(
    report.warnings.filter((entry) => entry.code === "unsupported-css-module-reference").length,
    2,
  );
  assert.ok(!report.changedFiles.includes("Conditional.vue"));
  assert.ok(!report.changedFiles.includes("Scripted.vue"));
});

test("scoped and module blocks migrate together in either order", async () => {
  const cwd = await fixture();
  const moduleFirst =
    '<template>\n  <p class="card" :class="$style.boxed">A</p>\n  <p class="etc">B</p>\n</template>\n<style module>\n.boxed { margin: 3px; }\n</style>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  const scopedFirst =
    '<template>\n  <p class="tone" :class="$style.pad">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.tone { color: red; }\n</style>\n<style module>\n.pad { padding: 5px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "AMixed.vue"), moduleFirst),
    writeFile(join(cwd, "ZMixed.vue"), scopedFirst),
  ]);
  await migrate({ cwd, write: true });
  assert.match(
    await readFile(join(cwd, "AMixed.vue"), "utf8"),
    /class="card m-\[3px\] p-\[13px\]">A/,
  );
  assert.doesNotMatch(await readFile(join(cwd, "AMixed.vue"), "utf8"), /<style/);
  assert.match(
    await readFile(join(cwd, "ZMixed.vue"), "utf8"),
    /class="tone p-\[5px\] text-\[red\]">A/,
  );
  assert.doesNotMatch(await readFile(join(cwd, "ZMixed.vue"), "utf8"), /<style/);
});

test("named module blocks retain with a warning", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p :class="classes.card">A</p>\n</template>\n<style module="classes">\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Named.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Named.vue" });
  assert.deepEqual(report.changedFiles, []);
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-sfc-block"));
});

test("preprocessor module blocks compile before conversion", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p :class="$style.card">A</p>\n  <p class="etc">B</p>\n</template>\n<style module lang="scss">\n$pad: 13px;\n.card { padding: $pad; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  await migrate({ cwd, styleFile: "Card.vue", write: true });
  const output = await readFile(join(cwd, "Card.vue"), "utf8");
  assert.match(output, /class="p-\[13px\]"/);
  // The variable declaration is not part of the deleted rule, so the
  // block stays behind with it -- matching scoped preprocessor behavior.
  assert.doesNotMatch(output, /\.card\s*[{\n]/);
});

test("module :global escapes retain individually while siblings convert", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p :class="$style.card">A</p>\n  <p class="etc">B</p>\n</template>\n<style module>\n.card { padding: 13px; }\n:global(.free) { color: blue; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(report.rules.some((rule) => rule.selector === ".card" && rule.status === "converted"));
  assert.ok(
    report.rules.some((rule) => rule.selector.includes(":global") && rule.status === "retained"),
  );
  assert.match(report.diff, /:global\(\.free\)/);
});

test("a dynamic directive argument naming $style retains the module", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p :class="$style.card">A</p>\n  <p v-on:[$style.card]="go">B</p>\n</template>\n<script setup>\nconst go = () => {};\n</script>\n<style module>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Dyn.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Dyn.vue" });
  assert.deepEqual(report.changedFiles, []);
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
});

test("an uneditable class attribute retains the module instead of duplicating it", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="plain&amp;x" :class="$style.card">A</p>\n  <p class="etc">B</p>\n</template>\n<style module>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Entity.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Entity.vue", write: true });
  assert.deepEqual(report.changedFiles, []);
  assert.ok(report.warnings.some((entry) => entry.code === "dynamic-template-class"));
  assert.match(await readFile(join(cwd, "Entity.vue"), "utf8"), /\$style\.card/);
});

test("open caller and dynamic surfaces retain modules like scoped rules", async () => {
  const cwd = await fixture();
  const singleRoot =
    '<template>\n  <p :class="$style.card">Single root</p>\n</template>\n<style module>\n.card { padding: 13px; }\n</style>\n';
  const spread =
    '<template>\n  <p v-bind="attrs" :class="$style.pad">A</p>\n  <p class="etc">B</p>\n</template>\n<script setup>\nconst attrs = { class: "danger" };\n</script>\n<style module>\n.pad { margin: 7px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Root.vue"), singleRoot),
    writeFile(join(cwd, "Spread.vue"), spread),
  ]);
  const report = await migrate({ cwd });
  assert.ok(!report.changedFiles.includes("Root.vue"));
  assert.ok(!report.changedFiles.includes("Spread.vue"));
  assert.ok(
    report.warnings.some(
      (entry) => entry.code === "open-root-fallthrough" && entry.file === "Root.vue",
    ),
  );
  assert.ok(
    report.warnings.some(
      (entry) => entry.code === "dynamic-template-class" && entry.file === "Spread.vue",
    ),
  );
});

test("a module compile failure never blocks the sibling scoped entry", async () => {
  const cwd = await fixture();
  // Both rules occupy the same block-local span, so path-level attribution
  // would block the healthy scoped rule too.
  const vue =
    '<template>\n  <p class="a">A</p>\n  <p :class="$style.bb">B</p>\n</template>\n<style scoped>\n.a { margin: 7px; }\n</style>\n<style module>\n.bb { COLOR: red; }\n</style>\n';
  await writeFile(join(cwd, "Split.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Split.vue" });
  assert.ok(report.rules.some((rule) => rule.selector === ".a" && rule.status === "converted"));
  assert.ok(report.rules.some((rule) => rule.selector === ".bb" && rule.status === "retained"));
  assert.ok(report.warnings.some((entry) => entry.code === "candidate-compilation-failure"));
  assert.match(report.diff, /class="a m-\[7px\]"/);
});

test("a blocked sibling rule keeps the shared $style binding alive", async () => {
  const cwd = await fixture();
  // The second rule fails Tailwind candidate compilation; the retained rule
  // stays unlayered, so converting the sibling would flip the cascade -- the
  // whole class retains and the binding survives untouched.
  const vue =
    '<template>\n  <p :class="$style.card">A</p>\n  <p class="etc">B</p>\n</template>\n<style module>\n.card { padding: 13px; }\n.card { COLOR: red; }\n</style>\n';
  await writeFile(join(cwd, "Blocked.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Blocked.vue", write: true });
  assert.deepEqual(report.changedFiles, []);
  assert.ok(report.rules.every((rule) => rule.selector === ".card" && rule.status === "retained"));
  assert.ok(report.warnings.some((entry) => entry.code === "candidate-compilation-failure"));
  assert.match(await readFile(join(cwd, "Blocked.vue"), "utf8"), /:class="\$style\.card"/);
});

test("module conflict warnings anchor to authored bytes after scoped edits", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="frame">A</p>\n  <p class="etc">C</p>\n  <p class="p-[10px]" :class="$style.boxed">B</p>\n</template>\n<style scoped>\n.frame { padding: 11px; }\n</style>\n<style module>\n.boxed { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Anchored.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Anchored.vue" });
  const warning = report.warnings.find((entry) => entry.code === "existing-tailwind-conflict");
  assert.ok(warning);
  assert.equal(vue.slice(warning.start, warning.end), "p-[10px]");
});

test("an unsupported module sibling block retains the whole $style module", async () => {
  const cwd = await fixture();
  // Every unnamed block feeds the same `$style` object, so deleting the
  // supported rule's binding would orphan a retained sibling's class.
  const prefix =
    '<template>\n  <p :class="$style.card">A</p>\n  <p class="etc">B</p>\n</template>\n<style module>\n.card { padding: 13px; }\n</style>\n';
  const siblings = [
    '<style module lang="postcss">\n.card { color: red; }\n</style>\n',
    '<style module src="./retained.css"></style>\n',
    '<style module media="screen">\n.card { color: red; }\n</style>\n',
    '<style module="$style">\n.card { color: red; }\n</style>\n',
  ];
  for (const sibling of siblings) {
    await writeFile(join(cwd, "Sibling.vue"), prefix + sibling);
    const report = await migrate({ cwd, styleFile: "Sibling.vue" });
    assert.deepEqual(report.changedFiles, []);
    assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
    assert.ok(report.rules.every((rule) => rule.status === "retained"));
  }
});

test("a surviving $style binding shadows scoped rules on its element", async () => {
  const cwd = await fixture();
  // The retained module rule competes unlayered with a replacement utility,
  // so the scoped rule on the shared element must retain; other elements
  // keep converting.
  const vue =
    '<template>\n  <p class="card" :class="$style.mod">A</p>\n  <p class="free">B</p>\n</template>\n<script setup>\nconst c = "blue";\n</script>\n<style scoped>\n.card { color: red; }\n.free { margin: 3px; }\n</style>\n<style module>\n.mod { color: v-bind(c); }\n</style>\n';
  await writeFile(join(cwd, "Shared.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Shared.vue" });
  const status = new Map(report.rules.map((rule) => [rule.selector, rule.status]));
  assert.equal(status.get(".card"), "retained");
  assert.equal(status.get(".free"), "converted");
  assert.equal(status.get(".mod"), "retained");
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
});

test("a retained module keeps co-located scoped rules retained", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card" :class="$style.mod">A</p>\n  <p class="etc">B</p>\n</template>\n<script setup>\nimport { useCssModule } from "vue";\nconst s = useCssModule();\nvoid s;\n</script>\n<style scoped>\n.card { color: red; }\n</style>\n<style module>\n.mod { color: blue; }\n</style>\n';
  await writeFile(join(cwd, "Closure.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Closure.vue", write: true });
  assert.deepEqual(report.changedFiles, []);
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-css-module-reference"));
});

test("a retained co-located scoped rule shadows the module entry", async () => {
  const cwd = await fixture();
  // On hover the later module selector originally ties the scoped selector's
  // specificity and wins by source order. A layered utility would lose to the
  // retained unlayered scoped rule instead, so both rules must stay authored.
  const vue =
    '<template>\n  <p class="card" :class="$style.mod">A</p>\n  <span>B</span>\n</template>\n<script setup>\nconst c = "red";\n</script>\n<style scoped>\n.card { color: v-bind(c); }\n</style>\n<style module>\n.mod:hover { color: blue; }\n</style>\n';
  await writeFile(join(cwd, "Cascade.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Cascade.vue" });
  assert.deepEqual(report.changedFiles, []);
  assert.ok(report.rules.every((rule) => rule.status === "retained"));
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
});

test("a retained co-located unscoped rule shadows the module entry", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="global" :class="$style.mod">A</p>\n  <span>B</span>\n</template>\n<script setup>\nconst c = "red";\n</script>\n<style>\n.global { color: v-bind(c); }\n</style>\n<style module>\n.mod { color: blue; }\n</style>\n';
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(join(cwd, "Cascade.vue"), vue),
  ]);
  const report = await migrate({ cwd });
  assert.deepEqual(report.changedFiles, []);
  assert.ok(report.rules.every((rule) => rule.status === "retained"));
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-value"));
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
});

test("retained Vue blocks prevent conflicting global at-rule moves", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <p class="card">Card</p>\n  <span>Leaf</span>\n</template>\n<style>\n@property --brand { syntax: "<color>"; inherits: false; initial-value: red; }\n</style>\n<style scoped>\n.card { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Button.module.css"),
      '@property /* docs */ --brand { syntax: "<color>"; inherits: false; initial-value: blue; }\n.button { padding: 13px; }\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  assert.equal(report.convertedRules, 2);
  assert.doesNotMatch(await readFile(join(cwd, "globals.css"), "utf8"), /@property --brand/);
  assert.match(await readFile(join(cwd, "Button.module.css"), "utf8"), /initial-value: blue/);
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /initial-value: red/);
});

test("opaque Vue registration identities prevent global at-rule moves", async () => {
  const cwd = await fixture({
    css: '@property --brand { syntax: "<color>"; inherits: false; initial-value: blue; }\n.button { padding: 13px; }\n',
  });
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p class="card">Card</p>\n  <span>Leaf</span>\n</template>\n<style lang="scss">\n$name: brand;\n@property --#{$name} { syntax: "<color>"; inherits: false; initial-value: red; }\n</style>\n<style scoped>\n.card { margin: 7px; }\n</style>\n',
  );
  const report = await migrate({ cwd, write: true });
  assert.equal(report.convertedRules, 1);
  assert.doesNotMatch(await readFile(join(cwd, "globals.css"), "utf8"), /@property --brand/);
  assert.match(await readFile(join(cwd, "Button.module.css"), "utf8"), /@property --brand/);
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /@property --#\{\$name\}/);
});

test("unsupported Vue style blocks prevent global at-rule moves", async () => {
  const cwd = await fixture({
    css: '@property --brand { syntax: "<color>"; inherits: false; initial-value: blue; }\n.button { padding: 13px; }\n',
  });
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <p class="card">Card</p>\n  <span>Leaf</span>\n</template>\n<style media="print">\n@property --brand { syntax: "<color>"; inherits: false; initial-value: red; }\n</style>\n<style scoped>\n.card { margin: 7px; }\n</style>\n',
  );
  const report = await migrate({ cwd, write: true });
  assert.equal(report.convertedRules, 1);
  assert.doesNotMatch(await readFile(join(cwd, "globals.css"), "utf8"), /@property --brand/);
  assert.match(await readFile(join(cwd, "Button.module.css"), "utf8"), /@property --brand/);
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /initial-value: red/);
});

test("a $style member without a module rule retains the whole module", async () => {
  const cwd = await fixture();
  // Deleting the only rule would empty the block, drop the runtime `$style`
  // injection, and crash the remaining member access.
  const vue =
    '<template>\n  <p :class="$style.card">A</p>\n  <p :class="$style.missing">B</p>\n</template>\n<style module>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Missing.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Missing.vue", write: true });
  assert.deepEqual(report.changedFiles, []);
  assert.ok(
    report.warnings.some(
      (entry) =>
        entry.code === "unsupported-css-module-reference" &&
        entry.message.includes("$style.missing"),
    ),
  );
  assert.match(await readFile(join(cwd, "Missing.vue"), "utf8"), /<style module>/);
});

test("caller classes on a module-bound child root join the cascade gate", async () => {
  const cwd = await fixture();
  const child =
    '<template>\n  <p :class="$style.card">Child root</p>\n</template>\n<style module>\n.card { padding: 13px; }\n</style>\n';
  const parent =
    '<template>\n  <Child class="danger" />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n';
  const clean =
    '<template>\n  <p :class="$style.pad">Plain root</p>\n</template>\n<style module>\n.pad { margin: 7px; }\n</style>\n';
  const cleanParent =
    '<template>\n  <Clean class="plain" />\n  <main>App2</main>\n</template>\n<script setup>\nimport Clean from "./Clean.vue";\n</script>\n';
  await Promise.all([
    writeFile(join(cwd, "Child.vue"), child),
    writeFile(join(cwd, "Parent.vue"), parent),
    writeFile(join(cwd, "Clean.vue"), clean),
    writeFile(join(cwd, "CleanParent.vue"), cleanParent),
    writeFile(join(cwd, "extra.css"), ".danger { padding: 20px; }\n"),
  ]);
  const report = await migrate({ cwd });
  const status = new Map(
    report.rules.map((rule) => [`${rule.file}:${rule.selector}`, rule.status]),
  );
  // `.danger` competes with the module rule on the fallthrough root, so it
  // retains; the sibling component without a competing corpus rule converts.
  assert.equal(status.get("Child.vue:.card"), "retained");
  assert.equal(status.get("Clean.vue:.pad"), "converted");
  assert.ok(
    report.warnings.some(
      (entry) => entry.code === "shadowed-scoped-rule" && entry.file === "Child.vue",
    ),
  );
});

test("checks effective child roots when retaining child scoped rules", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="foo">Child</div>\n</template>\n<style scoped>\n.foo { margin: 7px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      // Multi-root parent: the chained-root fallthrough gate must not
      // fire, so the effective-root shadow check is what retains the rule.
      '<template>\n  <Child class="extra" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(join(cwd, "globals.css"), '@import "tailwindcss";\n.extra { margin: 0; }\n'),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.ok(!report.changedFiles.includes("Child.vue"));
});

test("retains child ID rules when a caller overrides the root ID", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div id="root">Child</div>\n</template>\n<style scoped>\n#root { margin: 7px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Child id="override" />\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.ok(!report.changedFiles.includes("Child.vue"));
});

test("retains parent scoped rules when child class fallthrough is disabled", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Leaf.vue"),
      '<template>\n  <div class="leaf">Leaf</div>\n</template>\n<script setup>\ndefineOptions({ inheritAttrs: false });\n</script>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Leaf />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Leaf from "./Leaf.vue";\n</script>\n<style scoped>\n.leaf { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "component-class-target"));
  assert.ok(!report.changedFiles.includes("Parent.vue"));
});

test("retains parent scoped rules when child props can consume classes", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Leaf.vue"),
      '<template>\n  <div class="leaf">Leaf</div>\n</template>\n<script setup>\ndefineProps({ class: String });\n</script>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Leaf />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Leaf from "./Leaf.vue";\n</script>\n<style scoped>\n.leaf { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "component-class-target"));
  assert.ok(!report.changedFiles.includes("Parent.vue"));
});

test("uses the caller ID as the effective child-root selector", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(join(cwd, "Leaf.vue"), '<template>\n  <div id="root">Leaf</div>\n</template>\n'),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Leaf id="override" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Leaf from "./Leaf.vue";\n</script>\n<style scoped>\n#root { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(
    report.rules.some(
      (rule) =>
        rule.file === "Parent.vue" && rule.selector === "#root" && rule.status === "retained",
    ),
  );
  assert.ok(!report.changedFiles.includes("Parent.vue"));
});

test("opens component selector proofs for dynamic caller IDs", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(join(cwd, "Leaf.vue"), '<template>\n  <div id="root">Leaf</div>\n</template>\n'),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Leaf :id="override" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Leaf from "./Leaf.vue";\n</script>\n<style scoped>\n#root { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "dynamic-template-class"));
  assert.ok(!report.changedFiles.includes("Parent.vue"));
});

test("warns when a child-root utility conflicts at a rewritten call site", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Leaf.vue"),
      '<template>\n  <div class="leaf m-0">Leaf</div>\n</template>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Leaf />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Leaf from "./Leaf.vue";\n</script>\n<style scoped>\n.leaf { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "existing-tailwind-conflict"));
  assert.match(report.diff, /<Leaf class="m-\[7px\]" \/>/);
});

test("checks child-root sites before deleting parent scoped rules", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(join(cwd, "Leaf.vue"), '<template>\n  <div class="leaf">Leaf</div>\n</template>\n'),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Leaf />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Leaf from "./Leaf.vue";\n</script>\n<style scoped>\n.leaf { margin: 7px; }\n</style>\n',
    ),
    writeFile(join(cwd, "globals.css"), '@import "tailwindcss";\ndiv { margin: 0; }\n'),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.ok(!report.changedFiles.includes("Parent.vue"));
});

test("keeps classless child roots in type-selector shadow checks", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      "<template>\n  <div>Child</div>\n</template>\n<style scoped>\n.passed { margin: 7px; }\n</style>\n",
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Child class="passed" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(join(cwd, "globals.css"), '@import "tailwindcss";\ndiv { margin: 0; }\n'),
  ]);
  const report = await migrate({ cwd, styleFile: "Child.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.ok(!report.changedFiles.includes("Child.vue"));
  assert.doesNotMatch(report.diff, /m-\[7px\]/);
});

test("includes unselected child scoped rules in parent shadow checks", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="leaf">Child</div>\n</template>\n<style scoped>\n.leaf { margin: 0; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Child />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n<style scoped>\n.leaf { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd, styleFile: "Parent.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.ok(!report.changedFiles.includes("Parent.vue"));
});

test("keeps caller proofs open when an owned SFC cannot be analyzed", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      "<template>\n  <div>Child</div>\n</template>\n<style scoped>\n.passed { margin: 7px; }\n</style>\n",
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Child class="passed" />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n',
    ),
    writeFile(
      join(cwd, "Hidden.vue"),
      '<template>\n  <Child class="passed" />\n</template>\n<docs>unsupported</docs>\n',
    ),
  ]);
  const report = await migrate({ cwd, styleFile: "Child.vue" });
  assert.ok(
    report.rules.some(
      (rule) =>
        rule.file === "Child.vue" && rule.selector === ".passed" && rule.status === "retained",
    ),
  );
  assert.ok(!report.changedFiles.includes("Child.vue"));
});

test("retains root fallthrough for callers outside script setup", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(join(cwd, "main.ts"), 'import Child from "./Child.vue";\nvoid Child;\n'),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "open-root-fallthrough"));
  assert.match(report.diff, /class="passed p-\[13px\]"/);
  assert.match(report.diff, /\.passed \{ padding: 13px; \}/);
});

test("a vanished real Sass entry stays a fatal integrity error", async () => {
  const sass = await loadProjectSass(process.cwd());
  await mkdir(".tmp", { recursive: true });
  const missing = join(process.cwd(), ".tmp", "missing-entry.scss");
  await assert.rejects(compileSassEntry(sass, missing, ".a { padding: 1px; }"), /ENOENT/);
  // Virtual SFC block entries never exist on disk and must keep compiling.
  const virtual = await compileSassEntry(sass, missing, ".a { padding: 1px; }", {
    virtualEntry: true,
  });
  assert.match(virtual.css, /padding: 1px/);
});

test("slots defeat the unscoped sole-source proof", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <div class="shared">Card</div>\n  <slot />\n</template>\n<style>\n.shared { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "unscoped-style-block"));
  assert.equal(report.convertedRules, 0);
  assert.ok(!report.changedFiles.includes("Card.vue"));
});

test("a scan-excluded HTML shell defeats the unscoped sole-source proof", async () => {
  const cwd = await fixture();
  execFileSync("git", ["init", "-q"], { cwd });
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(join(cwd, ".gitignore"), "index.html\n"),
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <div class="shared">Card</div>\n  <span>Leaf</span>\n</template>\n<style>\n.shared { margin: 7px; }\n</style>\n',
    ),
    writeFile(join(cwd, "index.html"), '<div id="app" class="shared"></div>\n'),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "unscoped-style-block"));
  assert.equal(report.convertedRules, 0);
});

test("HTML script text resembling a style tag does not open the Vue shadow corpus", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <div class="card">Card</div>\n  <span>Leaf</span>\n</template>\n<style scoped>\n.card { margin: 7px; }\n</style>\n',
    ),
    writeFile(join(cwd, "index.html"), '<script>const tag = "<style>";</script>\n'),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.deepEqual(report.changedFiles, ["Card.vue"]);
  assert.doesNotMatch(await readFile(join(cwd, "Card.vue"), "utf8"), /<style/);
});

test("noscript style fallbacks open the Vue shadow corpus", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <div class="card">Card</div>\n  <span>Leaf</span>\n</template>\n<style scoped>\n.card { margin: 7px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "index.html"),
      "<noscript><style>.card { margin: 9px; }</style></noscript>\n",
    ),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("caller template edits do not recompile untouched preprocessor blocks", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="passed">Child</div>\n</template>\n<style scoped>\n.passed { padding: 13px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "App.vue"),
      '<template>\n  <Child class="passed" />\n  <main>App</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n<style scoped lang="scss">\n.app-box { margin: $undefined-size; }\n</style>\n',
    ),
  ]);
  // The caller's broken SCSS block was never selected or edited; migrating
  // the child must not force it through the compiler.
  const report = await migrate({ cwd, styleFile: "Child.vue", write: true });
  assert.deepEqual(
    report.warnings.filter((entry) => entry.code === "open-root-fallthrough"),
    [],
  );
  assert.match(await readFile(join(cwd, "App.vue"), "utf8"), /class="passed p-\[13px\]"/);
  assert.doesNotMatch(await readFile(join(cwd, "Child.vue"), "utf8"), /<style/);
});

test("migrates unscoped Vue styles when the package has one source", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <div class="shared">Card</div>\n  <span>Leaf</span>\n</template>\n<style>\n.shared { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  assert.deepEqual(
    report.warnings.filter((entry) => entry.code === "unscoped-style-block"),
    [],
  );
  assert.match(await readFile(join(cwd, "Card.vue"), "utf8"), /class="shared m-\[7px\]"/);
  assert.doesNotMatch(await readFile(join(cwd, "Card.vue"), "utf8"), /<style/);
});

test("retains unscoped Vue rules that can lose the unlayered cascade", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <div class="shared">Card</div>\n  <span>Leaf</span>\n</template>\n<style>\n.shared { margin: 7px; }\n</style>\n',
    ),
    writeFile(join(cwd, "globals.css"), '@import "tailwindcss";\ndiv { margin: 0; }\n'),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
});

test("keeps retained same-file style blocks in unscoped shadow checks", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <div class="shared extra">Card</div>\n  <span>Leaf</span>\n</template>\n<style lang="stylus">\n.extra { margin: 0; }\n</style>\n<style>\n.shared { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "preprocessor-style-block"));
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
});

test("retains unscoped Vue rules when another project source exists", async () => {
  const cwd = await fixture();
  await writeFile(
    join(cwd, "Card.vue"),
    '<template>\n  <div class="shared">Card</div>\n  <span>Leaf</span>\n</template>\n<style>\n.shared { margin: 7px; }\n</style>\n',
  );
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "unscoped-style-block"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
});

test("retains unscoped Vue rules when planning workspace packages", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    rm(join(cwd, "globals.css")),
    writeFile(join(cwd, "package.json"), '{"private":true,"workspaces":["packages/*"]}'),
    mkdir(join(cwd, "packages", "lib"), { recursive: true }),
    mkdir(join(cwd, "packages", "app"), { recursive: true }),
  ]);
  await Promise.all([
    writeFile(join(cwd, "packages", "lib", "package.json"), '{"private":true}'),
    writeFile(join(cwd, "packages", "lib", "globals.css"), '@import "tailwindcss";\n'),
    writeFile(
      join(cwd, "packages", "lib", "Card.vue"),
      '<template>\n  <div class="shared">Card</div>\n</template>\n<style>\n.shared { margin: 7px; }\n</style>\n',
    ),
    writeFile(join(cwd, "packages", "app", "package.json"), '{"private":true}'),
    writeFile(
      join(cwd, "packages", "app", "App.vue"),
      '<template>\n  <Card />\n  <div class="shared">App</div>\n</template>\n<script setup>\nimport Card from "../../lib/Card.vue";\n</script>\n',
    ),
  ]);
  execFileSync("git", ["init", "-q"], { cwd });
  const workspaceReport = await migrate({ cwd, workspaces: true });
  assert.ok(workspaceReport.warnings.some((entry) => entry.code === "unscoped-style-block"));
  assert.ok(!workspaceReport.changedFiles.includes("packages/lib/Card.vue"));
  const packageReport = await migrate({ cwd: join(cwd, "packages", "lib") });
  assert.ok(packageReport.warnings.some((entry) => entry.code === "unscoped-style-block"));
  assert.ok(!packageReport.changedFiles.includes("Card.vue"));
});

test("unresolved component roots block unscoped Vue deletion", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <ThirdParty />\n  <div class="shared">Card</div>\n</template>\n<style>\n.shared { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "unscoped-style-block"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
});

test("unresolved external styles block unscoped Vue deletion", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(
      join(cwd, "Card.vue"),
      '<template>\n  <div class="shared">Card</div>\n  <span>Leaf</span>\n</template>\n<style module src="@/theme.css"></style>\n<style>\n.shared { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  assert.ok(report.warnings.some((entry) => entry.code === "unsupported-sfc-block"));
  assert.ok(report.warnings.some((entry) => entry.code === "unscoped-style-block"));
  assert.ok(!report.changedFiles.includes("Card.vue"));
});

test("retains competing cross-file scoped rules with styles after templates", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(
      join(cwd, "AParent.vue"),
      '<template>\n  <ZChild />\n  <main>Parent</main>\n</template>\n<script setup>\nimport ZChild from "./ZChild.vue";\n</script>\n<style scoped>\n.child { margin: 7px; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "ZChild.vue"),
      '<template>\n  <div class="child own">Child</div>\n</template>\n<style scoped>\n.own { padding: 13px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  assert.equal(report.convertedRules, 0);
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  const childRuleStart = Buffer.byteLength(
    '<template>\n  <div class="child own">Child</div>\n</template>\n<style scoped>\n',
  );
  const childRule = report.rules.find(
    (rule) => rule.file === "ZChild.vue" && rule.selector === ".own",
  )!;
  assert.deepEqual(childRule.authoredSpan, {
    start: childRuleStart,
    end: childRuleStart + ".own { padding: 13px; }".length,
  });
  assert.doesNotMatch(await readFile(join(cwd, "ZChild.vue"), "utf8"), /p-\[13px\]/);
  // Mutually competing rules freeze without appends: deleting or
  // duplicating either side could flip the cascade.
  assert.doesNotMatch(await readFile(join(cwd, "AParent.vue"), "utf8"), /m-\[7px\]/);
});

test("retains competing cross-file scoped rules with styles before templates", async () => {
  const cwd = await fixture();
  await Promise.all([
    rm(join(cwd, "Button.module.css")),
    rm(join(cwd, "Button.tsx")),
    writeFile(
      join(cwd, "AChild.vue"),
      '<style scoped>\n.own { padding: 13px; }\n</style>\n<template>\n  <div class="child own">Child</div>\n</template>\n',
    ),
    writeFile(
      join(cwd, "ZParent.vue"),
      '<template>\n  <AChild />\n  <main>Parent</main>\n</template>\n<script setup>\nimport AChild from "./AChild.vue";\n</script>\n<style scoped>\n.child { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd, write: true });
  assert.equal(report.convertedRules, 0);
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.doesNotMatch(await readFile(join(cwd, "AChild.vue"), "utf8"), /p-\[13px\]/);
  assert.doesNotMatch(await readFile(join(cwd, "ZParent.vue"), "utf8"), /m-\[7px\]/);
});

test("retains a parent rule when an unselected child shadow is unverifiable", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "Child.vue"),
      '<template>\n  <div class="leaf">Leaf</div>\n</template>\n<style scoped lang="scss">\n$color: red;\n.note { color: $color; }\n</style>\n',
    ),
    writeFile(
      join(cwd, "Parent.vue"),
      '<template>\n  <Child />\n  <main>Parent</main>\n</template>\n<script setup>\nimport Child from "./Child.vue";\n</script>\n<style scoped>\n.leaf { margin: 7px; }\n</style>\n',
    ),
  ]);
  const report = await migrate({ cwd, styleFile: "Parent.vue" });
  assert.equal(report.diff, "");
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.ok(!report.changedFiles.includes("Child.vue"));
});

test("migrates scoped Vue preprocessor blocks through project compilers", async () => {
  for (const [lang, declaration, candidate] of [
    ["scss", "$space: 13px;\n.card { padding: $space; }", "p-[13px]"],
    ["sass", "$space: 13px\n.card\n  padding: $space", "p-[13px]"],
    ["less", "@space: 13px;\n.card { padding: @space; }", "p-[13px]"],
  ]) {
    const cwd = await fixture();
    const vue = `<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped lang="${lang}">\n${declaration}\n</style>\n`;
    await writeFile(join(cwd, "Card.vue"), vue);
    const report = await migrate({ cwd, styleFile: "Card.vue", write: true });
    assert.deepEqual(report.changedFiles, ["Card.vue"]);
    assert.equal(report.convertedRules, 1);
    assert.match(
      await readFile(join(cwd, "Card.vue"), "utf8"),
      new RegExp(`class="card ${candidate.replaceAll("[", "\\[").replaceAll("]", "\\]")}"`),
    );
    assert.doesNotMatch(await readFile(join(cwd, "Card.vue"), "utf8"), /\.card\s*[{\n]/);
  }
});

test("maps partial Vue preprocessor edits to absolute authored bytes", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p title="한글" class="card">A</p>\n  <p class="keep">B</p>\n</template>\n<style scoped lang="scss">\n.card { padding: 13px; }\n.keep { animation: spin 1s; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const first = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.deepEqual(first.changedFiles, ["Card.vue"]);
  const output = await readFile(join(cwd, "Card.vue"), "utf8");
  assert.match(output, /class="card p-\[13px\]"/);
  assert.doesNotMatch(output, /\.card \{ padding: 13px; \}/);
  assert.match(output, /\.keep \{ animation: spin 1s; \}/);
  const second = await migrate({ cwd, styleFile: "Card.vue", write: true });
  assert.deepEqual(second.changedFiles, []);
});

test("an external script block leaves scoped rules shadow-retained", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<script src="./behavior.js"></script>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "behavior.js"), "export default {};\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  // The external script's imports are unreadable, so the global CSS it may
  // load keeps the scoped rule from being deleted.
  assert.equal(report.convertedRules, 0);
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
});

test("a global type selector shadows scoped deletion only for matching tags", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "site.css"), "p { padding: 20px; }\n"),
  ]);
  const shadowed = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(shadowed.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(shadowed.convertedRules, 0);

  await writeFile(join(cwd, "site.css"), "article { padding: 20px; }\n");
  const clear = await migrate({ cwd, styleFile: "Card.vue" });
  assert.equal(clear.convertedRules, 1);
});

test("Sass parent-selector concatenation makes the shadow corpus unverifiable", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card-active">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card-active { padding: 13px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "theme.scss"), ".card { &-active { padding: 20px; } }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("ignores function ref runtime mutations outside static template scope", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p :ref="el => el?.classList.add(\'card\')">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.equal(report.convertedRules, 1);
  assert.equal(report.retainedRules, 0);
  assert.match(report.diff, /class="card p-\[13px\]"/);
});

test("ignores handler runtime mutations outside static template scope", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p @click="$event.currentTarget.classList.add(pick())">B</p>\n</template>\n<script setup>\nconst pick = () => "x";\n</script>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.equal(report.convertedRules, 1);
  assert.equal(report.retainedRules, 0);
  assert.match(report.diff, /class="card p-\[13px\]"/);
});

test("an unsupported script language leaves scoped rules shadow-retained", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<script lang="coffee">\nx = 1\n</script>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  // Unreadable script imports may load competing global CSS.
  assert.equal(report.convertedRules, 0);
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
});

test("a ::v-global escape in another SFC shadows scoped deletion", async () => {
  const cwd = await fixture();
  const child =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  const parent =
    '<template>\n  <div class="wrap">P</div>\n</template>\n<style scoped>\n::v-global(.card) { padding: 20px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Child.vue"), child),
    writeFile(join(cwd, "Parent.vue"), parent),
  ]);
  const report = await migrate({ cwd, styleFile: "Child.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("script blocks do not open the static template class surface", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<script setup>\nconst answer = 42;\n</script>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await writeFile(join(cwd, "Card.vue"), vue);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.equal(report.convertedRules, 1);
  assert.equal(report.retainedRules, 0);
  assert.match(report.diff, /class="card p-\[13px\]"/);
});

test("an external style block participates in scoped cascade shadowing", async () => {
  const cwd = await fixture();
  const vue =
    '<template>\n  <p class="card">A</p>\n  <p class="etc">B</p>\n</template>\n<style scoped src="./external.css"></style>\n<style scoped>\n.card { padding: 13px; }\n</style>\n';
  await Promise.all([
    writeFile(join(cwd, "Card.vue"), vue),
    writeFile(join(cwd, "external.css"), ".card { padding: 20px; }\n"),
  ]);
  const report = await migrate({ cwd, styleFile: "Card.vue" });
  assert.ok(report.warnings.some((entry) => entry.code === "shadowed-scoped-rule"));
  assert.equal(report.convertedRules, 0);
});

test("retains a CSS Module referenced by a Vue SFC script", async () => {
  const cwd = await fixture();
  const vue =
    "<template>\n  <button :class=\"styles.button\">Save</button>\n</template>\n<script setup>\nimport styles from './Button.module.css';\n</script>\n";
  await writeFile(join(cwd, "Button.vue"), vue);
  const report = await migrate({ cwd });
  const warning = report.warnings.find(
    (entry) => entry.code === "unsupported-css-module-reference",
  )!;
  assert.equal(warning.file, "Button.vue");
  assert.ok(!report.changedFiles.includes("Button.module.css"));
  assert.equal(report.retainedRules, 1);
});

test("withholds quote-bearing candidates from quoted HTML attributes and retains their rules", async () => {
  const cwd = await fixture();
  await Promise.all([
    writeFile(
      join(cwd, "site.css"),
      '.card { font-family: "My Font", sans-serif; }\n.btn { padding: 13px; }\n',
    ),
    writeFile(
      join(cwd, "index.html"),
      '<link rel="stylesheet" href="./site.css"><div class="card"></div><div class=\'btn\'></div>\n',
    ),
  ]);
  const report = await migrate({ cwd });
  // The double-quoted attribute cannot hold the quoted candidate; the
  // single-quoted one can, and the global rules stay retained as always.
  assert.ok(!report.diff.includes("My_Font"));
  assert.match(report.diff, /class='btn p-\[13px\]'/);
});

test("staging failure leaves originals intact and no temporary artifacts", async () => {
  const cwd = await tempDir();
  const goodPath = join(cwd, "good.css");
  const before = ".a { color: red; }\n";
  await writeFile(goodPath, before);
  await assert.rejects(
    writeChanges(
      [
        { path: goodPath, source: ".a { color: blue; }\n", before },
        { path: join(cwd, "missing.css"), source: "", before: "" },
      ],
      [],
    ),
    { code: "ENOENT" },
  );
  assert.equal(await readFile(goodPath, "utf8"), before);
  const leftovers = (await readdir(cwd)).filter((name) => name.includes(".tw-migrate-"));
  assert.deepEqual(leftovers, []);
});

test("preserves CRLF line endings through a partial migration", async () => {
  const cwd = await fixture({
    css: ".button {\r\n  padding: 13px;\r\n}\r\n.other {\r\n  display: grid;\r\n}\r\n",
    tsx: initialTsx.replaceAll("\n", "\r\n"),
  });
  await migrate({ cwd, styleFile: "Button.module.css", write: true });
  assert.deepEqual(
    await readFile(join(cwd, "Button.module.css")),
    Buffer.from("\r\n.other {\r\n  display: grid;\r\n}\r\n"),
  );
  assert.deepEqual(
    await readFile(join(cwd, "Button.tsx")),
    Buffer.from(
      "import styles from './Button.module.css';\r\nexport const Button = () => <button className=\"p-[13px]\">Save</button>;\r\n",
    ),
  );
});
