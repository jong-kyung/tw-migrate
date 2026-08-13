import { expect, test } from "vite-plus/test";

import { __unstable__loadDesignSystem as loadDesignSystem } from "tailwindcss";

import { canonicalCandidate } from "../src/plan/canonicalize.ts";
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
