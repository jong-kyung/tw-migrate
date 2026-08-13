// Per-candidate canonicalization against one loaded Tailwind design system.
// The bridge owns only the spelling lookup; declaration-shape and
// runtime-stability proofs stay with the caller. Results are cached per
// design-system instance because provisional entries (generated media
// variants, proposed font tokens) change the answer.

import type { DesignSystem } from "../types.ts";

const caches = new WeakMap<DesignSystem, Map<string, string | null>>();

/// The single canonical spelling Tailwind selects for one complete
/// candidate, or null when the target build lacks the capability, the
/// lookup fails, Tailwind returns no result or more than one result, or
/// the result is the unchanged input. A null keeps the original valid
/// arbitrary candidate; the bridge never guesses a named spelling.
export function canonicalCandidate(system: DesignSystem, candidate: string): string | null {
  if (typeof system.canonicalizeCandidates !== "function") return null;
  let cache = caches.get(system);
  if (cache === undefined) {
    cache = new Map();
    caches.set(system, cache);
  }
  const cached = cache.get(candidate);
  if (cached !== undefined) return cached;
  let canonical: string | null;
  try {
    // One candidate per call preserves a direct input-to-output mapping
    // because the method may deduplicate a batch.
    const results = system.canonicalizeCandidates([candidate]);
    canonical =
      results.length === 1 && typeof results[0] === "string" && results[0] !== candidate
        ? results[0]
        : null;
  } catch {
    canonical = null;
  }
  cache.set(candidate, canonical);
  return canonical;
}
