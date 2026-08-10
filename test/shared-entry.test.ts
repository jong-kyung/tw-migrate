import { expect, test } from "vite-plus/test";

import { proveSharedEntry, scanProof, tailwindEntryCatalog } from "../src/plan/entry.ts";
import type { PreparedSourceFile } from "../src/types.ts";

const root = "/repo";
const child = "/repo/packages/app";
const entry = "/repo/globals.css";

test("catalogs Tailwind entries by owning package", () => {
  const styleSources = new Map([
    [entry, '@import "tailwindcss";\n'],
    ["/repo/plain.css", ".a { color: red; }\n"],
    ["/repo/packages/app/own.css", '@import "tailwindcss" prefix(tw);\n'],
  ]);
  const owners = new Map<string, string | undefined>([
    [entry, root],
    ["/repo/plain.css", root],
    ["/repo/packages/app/own.css", child],
  ]);

  expect(tailwindEntryCatalog(styleSources, owners)).toEqual(
    new Map([
      [root, [entry]],
      [child, ["/repo/packages/app/own.css"]],
    ]),
  );
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
  const styleSources = new Map([["/repo/theme.css", '@source not "./packages/app";\n']]);
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
      styleSources: new Map([["/repo/theme.css", '@source "./packages/app";\n']]),
    }),
  ).toBe("literal");
  expect(prove('@import "tailwindcss";\n', "/repo/other/globals.css")).toBe(null);
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
    owned: (path) => path.startsWith(`${child}/`),
    writable: () => true,
    packageJson: options.packageJson ?? {},
    ignoredPaths: options.ignoredPaths ?? new Set(),
  });
}

const loader = file(
  `${child}/main.tsx`,
  "import '../../globals.css';\nimport { Button } from './Button.tsx';\n",
);
const consumer = file(
  `${child}/Button.tsx`,
  "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n",
);

test("proves consumers statically reachable from a loading source", () => {
  const proofs = prove({ packageSources: [loader, consumer] });

  expect(proofs).not.toBe(null);
  expect(proofs?.provenStyle(`${child}/Button.module.css`, [consumer])).toBe(true);
});

test("rejects a package that never loads the entry", () => {
  expect(prove({ packageSources: [consumer] })).toBe(null);
});

test("ignores entry paths outside real import statements", () => {
  const commented = file(
    `${child}/main.tsx`,
    "// import '../../globals.css';\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  );
  const deadString = file(
    `${child}/main.tsx`,
    "const doc = '../../globals.css';\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  );
  const blockCommented = file(
    `${child}/main.tsx`,
    "/* import '../../globals.css'; */\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  );

  expect(prove({ packageSources: [commented, consumer] })).toBe(null);
  expect(prove({ packageSources: [deadString, consumer] })).toBe(null);
  expect(prove({ packageSources: [blockCommented, consumer] })).toBe(null);
});

test("keeps consumers outside child ownership unproven", () => {
  // A sibling package's file consumes the child's stylesheet through a
  // cross-package import and is reachable from the child's loader, but it
  // can run under its own package without this entry's CSS.
  const bridge = file(
    `${child}/bridge.tsx`,
    "export { Foreign } from '../../sibling/Foreign.tsx';\n",
  );
  const loaderWithBridge = file(
    `${child}/main.tsx`,
    "import '../../globals.css';\nimport { Button } from './Button.tsx';\nexport { Foreign } from './bridge.tsx';\n",
  );
  const foreign = file(
    `${root}/sibling/Foreign.tsx`,
    "import styles from '../packages/app/Button.module.css';\nexport const Foreign = () => <i className={styles.button} />;\n",
  );
  const proofs = prove({ packageSources: [loaderWithBridge, bridge, consumer, foreign] });

  expect(proofs?.provenStyle(`${child}/Button.module.css`, [consumer, foreign])).toBe(false);
  expect(proofs?.provenStyle(`${child}/Button.module.css`, [consumer])).toBe(true);
});

test("a type-only entry import is not a loader", () => {
  const typeOnlyLoader = file(
    `${child}/main.tsx`,
    "import type {} from '../../globals.css';\nimport { Button } from './Button.tsx';\nexport const render = Button;\n",
  );

  expect(prove({ packageSources: [typeOnlyLoader, consumer] })).toBe(null);
});

test("a conventional index entry exposes its import closure", () => {
  const index = file(`${child}/index.ts`, "export { Button } from './Button.tsx';\n");
  const exposedByIndex = prove({ packageSources: [loader, consumer, index] });
  const noIndex = prove({ packageSources: [loader, consumer] });

  // Without export metadata the conventional index remains externally
  // loadable, so consumers behind it are exposed; a package with no index
  // exposes nothing by convention.
  expect(exposedByIndex?.provenStyle(`${child}/Button.module.css`, [consumer])).toBe(false);
  expect(noIndex?.provenStyle(`${child}/Button.module.css`, [consumer])).toBe(true);
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
    `${child}/main.tsx`,
    "import '../../globals.css';\nimport type { Button } from './Button.tsx';\nexport type { Button };\n",
  );
  const inlineTypeLoader = file(
    `${child}/main.tsx`,
    "import '../../globals.css';\nimport { type Button } from './Button.tsx';\n",
  );

  expect(
    prove({ packageSources: [typeLoader, consumer] })?.provenStyle(`${child}/Button.module.css`, [
      consumer,
    ]),
  ).toBe(false);
  expect(
    prove({ packageSources: [inlineTypeLoader, consumer] })?.provenStyle(
      `${child}/Button.module.css`,
      [consumer],
    ),
  ).toBe(false);
});

test("commented imports create no reachability edges", () => {
  const loaderWithoutEdge = file(
    `${child}/main.tsx`,
    "import '../../globals.css';\n// import { Button } from './Button.tsx';\n",
  );
  const proofs = prove({ packageSources: [loaderWithoutEdge, consumer] });

  expect(proofs?.provenStyle(`${child}/Button.module.css`, [consumer])).toBe(false);
});

test("keeps a consumer outside every proven flow retained", () => {
  const stray = file(
    `${child}/Stray.tsx`,
    "import styles from './Button.module.css';\nexport const Stray = () => <i className={styles.button} />;\n",
  );
  const proofs = prove({ packageSources: [loader, consumer, stray] });

  expect(proofs?.provenStyle(`${child}/Button.module.css`, [consumer, stray])).toBe(false);
});

test("treats exported consumers as unproven flows", () => {
  const proofs = prove({
    packageSources: [loader, consumer],
    packageJson: { main: "./main.tsx" },
  });

  // The consumer is reachable from the loading source, but the same chain
  // is exposed through the package entry point, so an external application
  // can render it without this entry's CSS.
  expect(proofs?.provenStyle(`${child}/Button.module.css`, [consumer])).toBe(false);
});

test("an unresolved declared entry point exposes every consumer", () => {
  const proofs = prove({
    packageSources: [loader, consumer],
    packageJson: { main: "./dist/index.js" },
  });

  expect(proofs?.provenStyle(`${child}/Button.module.css`, [consumer])).toBe(false);
});

test("wildcard exports expose every consumer", () => {
  const proofs = prove({
    packageSources: [loader, consumer],
    packageJson: { exports: { "./*": "./*" } },
  });

  expect(proofs?.provenStyle(`${child}/Button.module.css`, [consumer])).toBe(false);
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

  expect(literal?.provenStyle(`${child}/Button.module.css`, [consumer])).toBe(true);
  expect(automatic?.provenStyle(`${child}/Button.module.css`, [consumer])).toBe(false);
});

test("an html consumer linking the entry proves itself", () => {
  // Prepared HTML contexts keep only package-local links, so the ancestor
  // entry link is proven from the raw source.
  const page: PreparedSourceFile = file(
    `${child}/index.html`,
    '<link rel="stylesheet" href="../../globals.css"><button class="button"></button>',
  );
  const proofs = prove({ packageSources: [page] });

  expect(proofs?.provenStyle(`${child}/styles.css`, [page])).toBe(true);
});

test("a base tag leaves html entry links unproven", () => {
  const page = file(
    `${child}/index.html`,
    '<base href="/other/"><link rel="stylesheet" href="../../globals.css">',
  );

  expect(prove({ packageSources: [page] })).toBe(null);
});
