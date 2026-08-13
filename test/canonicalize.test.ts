import { expect, test } from "vite-plus/test";

import { __unstable__loadDesignSystem as loadDesignSystem } from "tailwindcss";

import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { acceptedCandidateAliases, canonicalCandidate } from "../src/plan/canonicalize.ts";
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
