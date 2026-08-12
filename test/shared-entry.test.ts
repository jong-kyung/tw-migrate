import { expect, test } from "vite-plus/test";

import { join, resolve, sep } from "node:path";

import { sourceAnalysis } from "../src/native.ts";
import { proveSharedEntry, scanProof, tailwindEntryCatalog } from "../src/plan/entry.ts";
import { indexStylesheetDependents } from "../src/util/shared.ts";
import type { PreparedSourceFile } from "../src/types.ts";

// Platform-resolved so separators and drive letters match what the proof
// helpers derive on Windows.
const root = resolve("/repo");
const child = join(root, "packages", "app");
const entry = join(root, "globals.css");

test("catalogs Tailwind entries by owning package", () => {
  const styleSources = new Map([
    [entry, '@import "tailwindcss";\n'],
    [join(root, "plain.css"), ".a { content: '@import \"tailwindcss\"'; }\n"],
    [join(child, "own.css"), '@import "tailwindcss" prefix(tw);\n'],
  ]);
  const owners = new Map<string, string | undefined>([
    [entry, root],
    [join(root, "plain.css"), root],
    [join(child, "own.css"), child],
  ]);

  expect(tailwindEntryCatalog(styleSources, owners)).toEqual(
    new Map([
      [root, [entry]],
      [child, [join(child, "own.css")]],
    ]),
  );
});

test("indexes parser-proven stylesheet dependencies", () => {
  const module = join(root, "Button.module.css");
  const importer = join(root, "consumer.scss");
  const stringOnly = join(root, "string.css");
  const dependents = indexStylesheetDependents(
    new Map([
      [module, ".button { padding: 1rem; }\n"],
      [importer, '@use "./Button.module";\n'],
      [stringOnly, ".x { content: '@import \"./Button.module.css\"'; }\n"],
    ]),
  );

  expect(dependents.get(module)).toEqual([importer]);
});

test("keeps dependency targets conservative when a stylesheet cannot parse", () => {
  const module = join(root, "Button.module.css");
  const opaque = join(root, "opaque.scss");
  const dependents = indexStylesheetDependents(
    new Map([
      [module, ".button { padding: 1rem; }\n"],
      [opaque, ".broken {\n"],
    ]),
  );

  expect(dependents.get(module)).toEqual([opaque]);
});

test("proves scan coverage through literal scopes and automatic bases", () => {
  const prove = (entrySource: string, entryPath = entry) =>
    scanProof({ entry: entryPath, entrySource, packageRoot: child });

  expect(prove('@import "tailwindcss";\n')).toBe("automatic");
  expect(prove('@import "tailwindcss";\n@source "./packages/app";\n')).toBe("literal");
  expect(prove('@import "tailwindcss";\n@source "./packages";\n')).toBe("literal");
  expect(prove('@import "tailwindcss" source(none);\n')).toBe(null);
  expect(prove('@import "tailwindcss" source(none);\n@source "./packages/app";\n')).toBe("literal");
  expect(prove('@import "tailwindcss" source("./packages/app");\n')).toBe("literal");
  expect(prove('@import "tailwindcss" source("./other");\n')).toBe(null);
  expect(prove('@import "tailwindcss";\n@source not "./packages/app";\n')).toBe(null);
  // A literal scope narrower than the package proves nothing for consumers
  // outside it: automatic detection still applies, and disabling detection
  // leaves the package unproven.
  expect(prove('@import "tailwindcss";\n@source "./packages/app/src";\n')).toBe("automatic");
  expect(prove('@import "tailwindcss" source(none);\n@source "./packages/app/src";\n')).toBe(null);
  expect(prove('@import "tailwindcss";\n@source not "./packages/app/tests";\n')).toBe(null);
  // Source directives in imported project CSS belong to the same entry
  // graph, resolved against their owning stylesheet; a graph import
  // missing from the corpus leaves the directives unknowable.
  const styleSources = new Map([[join(root, "theme.css"), '@source not "./packages/app";\n']]);
  expect(
    scanProof({
      entry,
      entrySource: '@import "tailwindcss";\n@import "./theme.css";\n',
      packageRoot: child,
      styleSources,
    }),
  ).toBe(null);
  expect(
    scanProof({
      entry,
      entrySource: '@import "tailwindcss";\n@import "./missing.css";\n',
      packageRoot: child,
      styleSources,
    }),
  ).toBe(null);
  expect(
    scanProof({
      entry,
      entrySource: '@import "tailwindcss";\n@import "./theme.css";\n',
      packageRoot: child,
      styleSources: new Map([[join(root, "theme.css"), '@source "./packages/app";\n']]),
    }),
  ).toBe("literal");
  expect(prove('@import "tailwindcss";\n', join(root, "other", "globals.css"))).toBe(null);
});

function file(path: string, source: string): PreparedSourceFile {
  return { path, source };
}

function prove(options: {
  packageSources: PreparedSourceFile[];
  packageJson?: Record<string, unknown>;
  ignoredPaths?: Set<string>;
  entrySource?: string;
}) {
  return proveSharedEntry({
    packageRoot: child,
    entry,
    entrySource: options.entrySource ?? '@import "tailwindcss";\n',
    packageSources: options.packageSources,
    owned: (path) => path.startsWith(`${child}${sep}`),
    writable: () => true,
    packageJson: options.packageJson ?? { private: true },
    ignoredPaths: options.ignoredPaths ?? new Set(),
  });
}

const loader = file(
  join(child, "main.tsx"),
  "import '../../globals.css';\nimport { Button } from './Button.tsx';\n",
);
const consumer = file(
  join(child, "Button.tsx"),
  "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n",
);

test("proves consumers statically reachable from a loading source", () => {
  const proofs = prove({ packageSources: [loader, consumer] });

  expect(proofs).not.toBe(null);
  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(true);
});

test("rejects a package that never loads the entry", () => {
  expect(prove({ packageSources: [consumer] })).toBe(null);
});

test("ignores entry paths outside real import statements", () => {
  const commented = file(
    join(child, "main.tsx"),
    "// import '../../globals.css';\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  );
  const deadString = file(
    join(child, "main.tsx"),
    "const doc = '../../globals.css';\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  );
  const blockCommented = file(
    join(child, "main.tsx"),
    "/* import '../../globals.css'; */\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  );

  expect(prove({ packageSources: [commented, consumer] })).toBe(null);
  expect(prove({ packageSources: [deadString, consumer] })).toBe(null);
  expect(prove({ packageSources: [blockCommented, consumer] })).toBe(null);
});

test("a bare package deep import of a child source exposes its closure", () => {
  const sibling = file(
    join(root, "sibling/Panel.tsx"),
    "import { Button } from '@acme/app/Button.tsx';\nexport const Panel = () => <Button />;\n",
  );
  const proofs = prove({
    packageSources: [loader, consumer, sibling],
    packageJson: { name: "@acme/app", private: true },
  });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("a foreign deep import of a child source exposes its closure", () => {
  // The sibling file imports the component directly; private prevents
  // publication but not in-repository deep imports, so the component runs
  // without this entry.
  const sibling = file(
    join(root, "sibling/Panel.tsx"),
    "import { Button } from '../packages/app/Button.tsx';\nexport const Panel = () => <Button />;\n",
  );
  const proofs = prove({ packageSources: [loader, consumer, sibling] });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("an entry-loading foreign importer exposes nothing", () => {
  // The root application both loads the entry and composes the child's
  // component, which is the natural monorepo topology.
  const rootMain = file(
    join(root, "src", "main.tsx"),
    "import '../globals.css';\nimport { Button } from '../packages/app/Button.tsx';\nexport const render = Button;\n",
  );
  const proofs = prove({ packageSources: [loader, consumer, rootMain] });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(true);
});

test("keeps consumers outside child ownership unproven", () => {
  // A sibling package's file consumes the child's stylesheet through a
  // cross-package import and is reachable from the child's loader, but it
  // can run under its own package without this entry's CSS.
  const bridge = file(
    join(child, "bridge.tsx"),
    "export { Foreign } from '../../sibling/Foreign.tsx';\n",
  );
  const loaderWithBridge = file(
    join(child, "main.tsx"),
    "import '../../globals.css';\nimport { Button } from './Button.tsx';\nexport { Foreign } from './bridge.tsx';\n",
  );
  const foreign = file(
    join(root, "sibling/Foreign.tsx"),
    "import styles from '../packages/app/Button.module.css';\nexport const Foreign = () => <i className={styles.button} />;\n",
  );
  const proofs = prove({ packageSources: [loaderWithBridge, bridge, consumer, foreign] });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer, foreign])).toBe(false);
  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(true);
});

test("a type-only entry import is not a loader", () => {
  const typeOnlyLoader = file(
    join(child, "main.tsx"),
    "import type {} from '../../globals.css';\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  );

  expect(prove({ packageSources: [typeOnlyLoader, consumer] })).toBe(null);
});

test("a conditional dynamic entry import is not a loader", () => {
  const conditionalLoader = file(
    join(child, "main.tsx"),
    "declare const enabled: boolean;\nif (enabled) import('../../globals.css');\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  );

  expect(prove({ packageSources: [conditionalLoader, consumer] })).toBe(null);
});

test("a disabled html link is not a loader", () => {
  const page = file(
    join(child, "index.html"),
    '<link rel="stylesheet" href="../../globals.css" disabled><button class="button"></button>',
  );

  expect(prove({ packageSources: [page] })).toBe(null);
});

test("a commented-out vue script block is not a loader", () => {
  const commented = file(
    join(child, "App.vue"),
    "<template><div /></template>\n<!-- <script>import '../../globals.css';</script> -->\n<script setup>import { Button } from './Button.tsx';</script>\n",
  );

  expect(prove({ packageSources: [commented, consumer] })).toBe(null);
});

test("wildcard positive scopes prove no literal coverage", () => {
  expect(
    scanProof({
      entry,
      entrySource: '@import "tailwindcss" source(none);\n@source "./packages/app/**/*.tsx";\n',
      packageRoot: child,
    }),
  ).toBe(null);
});

test("a prepared vue loader carries records parsed with its script language", () => {
  const script =
    "import '../../globals.css';\nimport { Button } from './Button.tsx';\nconst render = () => <Button />;\n";
  const vueLoader = {
    ...file(
      join(child, "App.vue"),
      `<script setup lang="tsx">\n${script}</script>\n<template><div /></template>\n`,
    ),
    sourceImports: sourceAnalysis("App.vue.tsx", script).imports,
  };
  const proofs = prove({ packageSources: [vueLoader, consumer] });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(true);
});

test("emitted javascript specifiers expose their typescript sources", () => {
  const index = file(join(child, "index.ts"), "export { Button } from './Button.js';\n");
  const proofs = prove({ packageSources: [loader, consumer, index] });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("an unresolved script import in the exposed closure exposes everything", () => {
  const index = file(
    join(child, "index.ts"),
    "export { hidden } from './generated.js';\nexport { Button } from './Button.tsx';\n",
  );
  const proofs = prove({ packageSources: [loader, consumer, index] });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("unquoted url imports join the proof graph", () => {
  const styleSources = new Map([[join(root, "theme.css"), '@source not "./packages/app";\n']]);

  expect(
    scanProof({
      entry,
      entrySource: '@import "tailwindcss";\n@import url(./theme.css);\n',
      packageRoot: child,
      styleSources,
    }),
  ).toBe(null);
});

test("tailwind subpath imports carry their source modifier", () => {
  expect(
    scanProof({
      entry,
      entrySource: '@import "tailwindcss/utilities" source(none);\n',
      packageRoot: child,
    }),
  ).toBe(null);
});

test("a conventional index entry exposes its import closure", () => {
  const index = file(join(child, "index.ts"), "export { Button } from './Button.tsx';\n");
  const exposedByIndex = prove({ packageSources: [loader, consumer, index] });
  const noIndex = prove({ packageSources: [loader, consumer] });

  // Without export metadata the conventional index remains externally
  // loadable, so consumers behind it are exposed; a package with no index
  // exposes nothing by convention.
  expect(exposedByIndex?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
  expect(noIndex?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(true);
});

test("directive-shaped text inside css strings proves nothing", () => {
  expect(
    scanProof({
      entry,
      entrySource:
        '@import "tailwindcss" source(none);\n.rule { content: \'@source "./packages/app"\'; }\n',
      packageRoot: child,
    }),
  ).toBe(null);
});

test("type-only imports create no reachability edges", () => {
  const typeLoader = file(
    join(child, "main.tsx"),
    "import '../../globals.css';\nimport type { Button } from './Button.tsx';\nexport type { Button };\n",
  );
  const inlineTypeLoader = file(
    join(child, "main.tsx"),
    "import '../../globals.css';\nimport { type Button } from './Button.tsx';\n",
  );

  expect(
    prove({ packageSources: [typeLoader, consumer] })?.provenStyle(
      join(child, "Button.module.css"),
      [consumer],
    ),
  ).toBe(false);
  expect(
    prove({ packageSources: [inlineTypeLoader, consumer] })?.provenStyle(
      join(child, "Button.module.css"),
      [consumer],
    ),
  ).toBe(false);
});

test("commented imports create no reachability edges", () => {
  const loaderWithoutEdge = file(
    join(child, "main.tsx"),
    "import '../../globals.css';\n// import { Button } from './Button.tsx';\n",
  );
  const proofs = prove({ packageSources: [loaderWithoutEdge, consumer] });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("keeps a consumer outside every proven flow retained", () => {
  const stray = file(
    join(child, "Stray.tsx"),
    "import styles from './Button.module.css';\nexport const Stray = () => <i className={styles.button} />;\n",
  );
  const proofs = prove({ packageSources: [loader, consumer, stray] });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer, stray])).toBe(false);
});

test("treats exported consumers as unproven flows", () => {
  const proofs = prove({
    packageSources: [loader, consumer],
    packageJson: { main: "./main.tsx" },
  });

  // The consumer is reachable from the loading source, but the same chain
  // is exposed through the package entry point, so an external application
  // can render it without this entry's CSS.
  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("an unresolved declared entry point exposes every consumer", () => {
  const proofs = prove({
    packageSources: [loader, consumer],
    packageJson: { main: "./dist/index.js" },
  });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("a publishable package without exports exposes every consumer", () => {
  const open = prove({ packageSources: [loader, consumer], packageJson: {} });
  const encapsulated = prove({
    packageSources: [loader, consumer],
    packageJson: { exports: { ".": "./main.tsx" } },
  });

  // Deep imports reach any subpath without an encapsulating exports map,
  // while the exports map confines exposure to its targets.
  expect(open?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
  expect(encapsulated?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("a css module-script import is not a loader", () => {
  const moduleScript = file(
    join(child, "main.tsx"),
    "import sheet from '../../globals.css' with { type: 'css' };\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  );

  expect(prove({ packageSources: [moduleScript, consumer] })).toBe(null);
});

test("a sheet without the utilities layer is not an entry", () => {
  const styleSources = new Map([
    [entry, '@import "tailwindcss";\n'],
    [join(root, "tokens.css"), '@import "tailwindcss/theme";\n'],
    [join(root, "split.css"), '@import "tailwindcss/theme";\n@import "tailwindcss/utilities";\n'],
  ]);
  const owners = new Map<string, string | undefined>([
    [entry, root],
    [join(root, "tokens.css"), root],
    [join(root, "split.css"), root],
  ]);

  expect(tailwindEntryCatalog(styleSources, owners)).toEqual(
    new Map([[root, [entry, join(root, "split.css")].sort()]]),
  );
});

test("string content never catalogs a tailwind entry", () => {
  const styleSources = new Map([
    [entry, '@import "tailwindcss";\n'],
    [join(root, "fake.css"), `.x { content: '@import "tailwindcss"'; }\n`],
  ]);
  const owners = new Map<string, string | undefined>([
    [entry, root],
    [join(root, "fake.css"), root],
  ]);

  expect(tailwindEntryCatalog(styleSources, owners)).toEqual(new Map([[root, [entry]]]));
});

test("a browser entry point exposes its import closure", () => {
  const browserEntry = file(join(child, "main.tsx"), loader.source);
  const proofs = prove({
    packageSources: [browserEntry, consumer],
    packageJson: { private: true, browser: "./main.tsx" },
  });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("wildcard exports expose every consumer", () => {
  const proofs = prove({
    packageSources: [loader, consumer],
    packageJson: { exports: { "./*": "./*" } },
  });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("automatic scan coverage requires consumers to pass ignore rules", () => {
  const literal = prove({
    packageSources: [loader, consumer],
    entrySource: '@import "tailwindcss";\n@source "./packages/app";\n',
    ignoredPaths: new Set([consumer.path]),
  });
  const automatic = prove({
    packageSources: [loader, consumer],
    ignoredPaths: new Set([consumer.path]),
  });

  expect(literal?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(true);
  expect(automatic?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("an html consumer linking the entry proves itself", () => {
  // Prepared HTML contexts keep only package-local links, so the ancestor
  // entry link is proven from the raw source.
  const page: PreparedSourceFile = file(
    join(child, "index.html"),
    '<link rel="stylesheet" href="../../globals.css"><button class="button"></button>',
  );
  const proofs = prove({ packageSources: [page] });

  expect(proofs?.provenStyle(join(child, "styles.css"), [page])).toBe(true);
});

test("commented-out html links are not loaders", () => {
  const page = file(
    join(child, "index.html"),
    '<!-- <link rel="stylesheet" href="../../globals.css"> --><button class="button"></button>',
  );

  expect(prove({ packageSources: [page] })).toBe(null);
});

test("a media-conditioned entry link is not an unconditional loader", () => {
  const printOnly = file(
    join(child, "index.html"),
    '<link rel="stylesheet" href="../../globals.css" media="print"><button class="button"></button>',
  );
  const all = file(
    join(child, "index.html"),
    '<link rel="stylesheet" href="../../globals.css" media="all"><button class="button"></button>',
  );

  expect(prove({ packageSources: [printOnly] })).toBe(null);
  expect(prove({ packageSources: [all] })).not.toBe(null);
});

test("an unresolved package alias in the exposed closure exposes everything", () => {
  const index = file(
    join(child, "index.ts"),
    "export { hidden } from '#internal/button';\nexport { Button } from './Button.tsx';\n",
  );
  const proofs = prove({ packageSources: [loader, consumer, index] });

  expect(proofs?.provenStyle(join(child, "Button.module.css"), [consumer])).toBe(false);
});

test("an unresolvable bare css import leaves the scan proof unproven", () => {
  expect(
    scanProof({
      entry,
      entrySource: '@import "tailwindcss";\n@import "@acme/theme";\n',
      packageRoot: child,
    }),
  ).toBe(null);
});

test("a base tag leaves html entry links unproven", () => {
  const page = file(
    join(child, "index.html"),
    '<base href="/other/"><link rel="stylesheet" href="../../globals.css">',
  );

  expect(prove({ packageSources: [page] })).toBe(null);
});
