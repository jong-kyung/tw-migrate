// Font theme-token naming and reuse for canonicalization. The planner
// supplies parsed family stacks; this module owns the deterministic name
// derivation and the targeted lookup of existing `--font-*` tokens, while
// provisional registration and safety proofs stay with the caller.

import { fontFamilyStack } from "../native.ts";

/// The RFC's deterministic token name for one decoded family name: NFC
/// normalization, lowercasing, one hyphen per run outside letters and
/// numbers, and stable fallbacks for empty and digit-leading results.
export function fontTokenName(family: string): string {
  const collapsed = family
    .normalize("NFC")
    .toLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, "-")
    .replace(/^-+|-+$/g, "");
  if (collapsed === "") return "family";
  return /^\p{N}/u.test(collapsed) ? `family-${collapsed}` : collapsed;
}

/// The RFC's bounded allocation sequence for one base name: the base
/// spelling, then numeric suffixes from -2, at most 100 spellings total.
/// Exhaustion is the caller's font-theme-registration-failed outcome.
export function fontNameCandidates(base: string): string[] {
  const names = [base];
  for (let suffix = 2; names.length < 100; suffix += 1) {
    names.push(`${base}-${suffix}`);
  }
  return names;
}

/// Existing `--font-*` theme tokens whose parsed stack equals the probe's
/// normalized stack, as utility candidate names sorted lexicographically.
/// The comparison runs both values through the planner's stack parser, so
/// authored spacing and quoting differences cannot defeat reuse, and an
/// unreadable token value never matches.
export function matchingFontTokens(
  themeTokens: Record<string, string>,
  normalizedStack: string,
): string[] {
  const matches: string[] = [];
  for (const [token, value] of Object.entries(themeTokens)) {
    if (!token.startsWith("font-")) continue;
    const parsed = fontFamilyStack(value);
    if (parsed !== null && parsed.value === normalizedStack) {
      matches.push(token);
    }
  }
  matches.sort();
  return matches;
}
