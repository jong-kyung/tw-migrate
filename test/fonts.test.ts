import { expect, test } from "vite-plus/test";

import { fontTokenName, matchingFontTokens } from "../src/plan/fonts.ts";

test("derives deterministic token names from family names", () => {
  expect(fontTokenName("Open Sans")).toBe("open-sans");
  expect(fontTokenName("Acme Display")).toBe("acme-display");
  expect(fontTokenName("Bold")).toBe("bold");
  // Runs outside letters and numbers collapse to one hyphen.
  expect(fontTokenName("IBM Plex Mono!  v2")).toBe("ibm-plex-mono-v2");
  expect(fontTokenName("---")).toBe("family");
  expect(fontTokenName("")).toBe("family");
  // A leading digit gains the family- prefix.
  expect(fontTokenName("3M Circular")).toBe("family-3m-circular");
  // Unicode letters survive NFC-normalized and lowercased.
  expect(fontTokenName("Núnito")).toBe("núnito");
});

test("matches existing font tokens through the stack parser", () => {
  const tokens = {
    "font-brand": '"Open Sans", sans-serif',
    // Authored spacing and quoting differences still match.
    "font-legacy": "  'Open Sans' ,sans-serif",
    "font-other": '"Roboto", sans-serif',
    "font-runtime": "var(--font-body)",
    "color-primary": '"Open Sans", sans-serif',
  };
  expect(matchingFontTokens(tokens, '"Open Sans", sans-serif')).toEqual([
    "font-brand",
    "font-legacy",
  ]);
  expect(matchingFontTokens(tokens, '"Nowhere", serif')).toEqual([]);
});
