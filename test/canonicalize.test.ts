import { expect, test } from "vite-plus/test";

import { __unstable__loadDesignSystem as loadDesignSystem } from "tailwindcss";

import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

import {
  acceptedCandidateAliases,
  canonicalCandidate,
  spellingReservations,
} from "../src/plan/canonicalize.ts";
import { loadTailwind } from "../src/tailwind.ts";
import type { DesignSystem } from "../src/types.ts";

async function system(css = "@tailwind utilities;"): Promise<DesignSystem> {
  return (await loadDesignSystem(css)) as unknown as DesignSystem;
}

test("canonicalizes arbitrary spellings to named utilities", async () => {
  const loaded = await system();
  expect(canonicalCandidate(loaded, "mr-[auto]")).toBe("mr-auto");
  expect(canonicalCandidate(loaded, "max-w-[100%]")).toBe("max-w-full");
});

test("preserves variants and important modifiers in canonical spellings", async () => {
  const loaded = await system();
  expect(canonicalCandidate(loaded, "hover:mr-[auto]")).toBe("hover:mr-auto");
  expect(canonicalCandidate(loaded, "mr-[auto]!")).toBe("mr-auto!");
});

test("keeps candidates without a named equivalent", async () => {
  const loaded = await system();
  expect(canonicalCandidate(loaded, "p-[13px]")).toBe(null);
  expect(canonicalCandidate(loaded, "mr-auto")).toBe(null);
});

test("canonicalizes generated media variants against their provisional definition", async () => {
  const plain = await system();
  expect(canonicalCandidate(plain, "twm-media-print:mr-[auto]")).toBe(null);
  const provisional = await system(
    "@custom-variant twm-media-print (@media print);\n@tailwind utilities;",
  );
  expect(canonicalCandidate(provisional, "twm-media-print:mr-[auto]")).toBe(
    "twm-media-print:mr-auto",
  );
  // Instance-scoped caches: the earlier system still answers from its own
  // definitions after the provisional lookup succeeded.
  expect(canonicalCandidate(plain, "twm-media-print:mr-[auto]")).toBe(null);
});

test("a missing canonicalization capability preserves the original candidate", async () => {
  const loaded = await system();
  const limited: DesignSystem = {
    theme: loaded.theme,
    candidatesToCss: (candidates) => loaded.candidatesToCss(candidates),
  };
  expect(canonicalCandidate(limited, "mr-[auto]")).toBe(null);
});

test("stylesheet references calibrate reservation boundedness", () => {
  // Platform-native absolute paths, because reference resolution runs
  // through node:path and drive-letter normalization on Windows would
  // never match a hard-coded POSIX key.
  const app = resolve("/app");
  // A relative import resolving into the snapshot stays bounded.
  expect(
    spellingReservations(
      new Map([
        [join(app, "main.css"), '@import "./theme.css";\n.card { color: red; }\n'],
        [join(app, "theme.css"), ".hero { color: blue; }\n"],
      ]),
      [],
    ),
  ).toEqual({ names: new Set(["card", "hero"]), prefixes: new Set(), unbounded: false });
  // The Tailwind package emits utilities, never authored selectors.
  expect(
    spellingReservations(new Map([[join(app, "globals.css"), '@import "tailwindcss";\n']]), [])
      .unbounded,
  ).toBe(false);
  // Sass built-in modules define functions without loading any sheet.
  expect(
    spellingReservations(
      new Map([["/app/main.scss", '@use "sass:math";\n.card { width: math.div(4, 2); }\n']]),
      [],
    ).unbounded,
  ).toBe(false);
  // A package or otherwise unresolved import loads unseen selectors.
  expect(
    spellingReservations(
      new Map([[join(app, "main.css"), '@import "legacy-package/theme.css";\n']]),
      [],
    ).unbounded,
  ).toBe(true);
  // An import the Tailwind loader already resolved into the extras is
  // covered by the extras' own selector scan, but only from the importer
  // location the loader resolved it for.
  const resolved = new Set([`${app}\0some-kit/styles.css`]);
  const extras: [string, string][] = [
    [`${app}\0some-kit/styles.css.graph.css`, ".kit { color: red; }\n"],
  ];
  expect(
    spellingReservations(
      new Map([[join(app, "globals.css"), '@import "some-kit/styles.css";\n']]),
      extras,
      resolved,
    ),
  ).toEqual({ names: new Set(["kit"]), prefixes: new Set(), unbounded: false });
  expect(
    spellingReservations(
      new Map([[join(app, "nested", "pkg", "legacy.css"), '@import "some-kit/styles.css";\n']]),
      extras,
      resolved,
    ).unbounded,
  ).toBe(true);
});

test("entry-graph directives calibrate reservation boundedness", () => {
  const bounded = (entry: string) =>
    spellingReservations(new Map(), [["/app/globals.css.graph.css", entry]]).unbounded === false;
  // Plugin and legacy config modules can emit selectors this scan never
  // sees, and a `@source not` prelude can exclude a canonical spelling
  // from generation.
  expect(bounded('@import "tailwindcss";\n')).toBe(true);
  expect(bounded('@import "tailwindcss";\n@plugin "./plugin.js";\n')).toBe(false);
  expect(bounded('@import "tailwindcss";\n@config "./tailwind.config.js";\n')).toBe(false);
  expect(bounded('@import "tailwindcss";\n@source not inline("mr-auto");\n')).toBe(false);
  expect(bounded('@import "tailwindcss";\n@source not "./legacy";\n')).toBe(false);
  // An additive inline safelist only generates extra utilities.
  expect(bounded('@import "tailwindcss";\n@source inline("mr-auto");\n')).toBe(true);
  // A source(...) import modifier disables or restricts automatic
  // scanning, so generation no longer follows the migrated sources.
  expect(bounded('@import "tailwindcss" source(none);\n')).toBe(false);
  expect(bounded('@import "tailwindcss" source("./apps");\n')).toBe(false);
});

test("alias acceptance rejects theme-backed and reserved spellings", async () => {
  const dir = await mkdtemp(join(tmpdir(), "tw-canonicalize-"));
  await writeFile(join(dir, "globals.css"), '@import "tailwindcss";\n');
  // The repository itself is the target project: Tailwind v4 resolves from
  // its node_modules exactly like a migrated project's own installation.
  const entry = await loadTailwind(process.cwd(), join(dir, "globals.css"), new Map(), dir);
  const open = { names: new Set<string>(), prefixes: new Set<string>(), unbounded: false };

  // p-4 dereferences --spacing, which literal-only runtime stability
  // rejects until the reservation scan lands with font registration.
  expect(acceptedCandidateAliases(entry.designSystem, ["p-[1rem]", "mr-[auto]"], open)).toEqual({
    "mr-[auto]": "mr-auto",
  });
  // text-sm carries a companion line-height declaration, so its shape
  // differs from the bare font-size source rule.
  expect(acceptedCandidateAliases(entry.designSystem, ["text-[0.875rem]"], open)).toEqual({});
  // An authored selector owns the canonical spelling.
  expect(
    acceptedCandidateAliases(entry.designSystem, ["mr-[auto]"], {
      names: new Set(["mr-auto"]),
      prefixes: new Set<string>(),
      unbounded: false,
    }),
  ).toEqual({});
  // An unbounded reservation rejects every alias.
  expect(
    acceptedCandidateAliases(entry.designSystem, ["mr-[auto]"], {
      names: new Set<string>(),
      prefixes: new Set<string>(),
      unbounded: true,
    }),
  ).toEqual({});
});

test("entry utility overrides reject canonical aliases", async () => {
  const dir = await mkdtemp(join(tmpdir(), "tw-canonicalize-"));
  // The override owns the mr-auto spelling with its own declarations, so
  // the entry-graph reservation keeps the arbitrary form.
  await writeFile(
    join(dir, "globals.css"),
    '@import "tailwindcss";\n@utility mr-auto {\n  margin-right: 1px;\n}\n',
  );
  const entry = await loadTailwind(process.cwd(), join(dir, "globals.css"), new Map(), dir);
  const reservations = spellingReservations(
    new Map(),
    entry.graphSources.map((graphSource) => [`${graphSource.path}.graph.css`, graphSource.source]),
  );
  expect(reservations.names.has("mr-auto")).toBe(true);
  expect(acceptedCandidateAliases(entry.designSystem, ["mr-[auto]"], reservations)).toEqual({});
});

test("prefixed entry utility overrides reject canonical aliases", async () => {
  const dir = await mkdtemp(join(tmpdir(), "tw-canonicalize-"));
  // The utility emits tw:mr-auto while the stylesheet scan records the
  // authored mr-auto prelude; the unprefixed lookup must still reserve.
  await writeFile(
    join(dir, "globals.css"),
    '@import "tailwindcss" prefix(tw);\n@utility mr-auto {\n  margin-right: 1px;\n}\n',
  );
  const entry = await loadTailwind(process.cwd(), join(dir, "globals.css"), new Map(), dir);
  const reservations = spellingReservations(
    new Map(),
    entry.graphSources.map((graphSource) => [`${graphSource.path}.graph.css`, graphSource.source]),
  );
  expect(reservations.names.has("mr-auto")).toBe(true);
  expect(acceptedCandidateAliases(entry.designSystem, ["tw:mr-[auto]"], reservations)).toEqual({});
  // The utility also owns its root beneath variant chains and the
  // important modifier.
  expect(
    acceptedCandidateAliases(entry.designSystem, ["tw:hover:mr-[auto]"], reservations),
  ).toEqual({});
  expect(acceptedCandidateAliases(entry.designSystem, ["tw:mr-[auto]!"], reservations)).toEqual({});
});

test("constrained dynamic class prefixes and variant brackets calibrate acceptance", async () => {
  const dir = await mkdtemp(join(tmpdir(), "tw-canonicalize-"));
  await writeFile(join(dir, "globals.css"), '@import "tailwindcss";\n');
  const entry = await loadTailwind(process.cwd(), join(dir, "globals.css"), new Map(), dir);

  // A template such as `mr-${value}` can complete to the canonical name.
  expect(
    acceptedCandidateAliases(entry.designSystem, ["mr-[auto]"], {
      names: new Set<string>(),
      prefixes: new Set(["mr-"]),
      unbounded: false,
    }),
  ).toEqual({});
  // Brackets inside a preserved arbitrary variant do not reject the named
  // utility segment.
  expect(
    acceptedCandidateAliases(entry.designSystem, ["[@media_(min-width:48rem)]:mr-[auto]"], {
      names: new Set<string>(),
      prefixes: new Set<string>(),
      unbounded: false,
    }),
  ).toEqual({ "[@media_(min-width:48rem)]:mr-[auto]": "[@media_(min-width:48rem)]:mr-auto" });
});
