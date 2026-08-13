// Per-candidate canonicalization against one loaded Tailwind design system.
// The bridge owns only the spelling lookup; declaration-shape and
// runtime-stability proofs stay with the caller. Results are cached per
// design-system instance because provisional entries (generated media
// variants, proposed font tokens) change the answer.

import { compiledShape, stylesheetAnalysis } from "../native.ts";
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

export interface SpellingReservations {
  /// Authored class spellings collected from every stylesheet snapshot.
  names: Set<string>;
  /// True when any snapshot's class match set cannot be bounded (an
  /// unparseable sheet, an interpolated class, or an attribute matcher
  /// beyond word equality); every alias is then rejected.
  unbounded: boolean;
}

/// Authored-selector reservations over the complete stylesheet snapshot.
/// Migration stylesheets never join the Tailwind scan corpus, so a retained
/// legacy rule such as `.mr-auto` would silently activate on a migrated
/// element unless its spelling is reserved here.
export function spellingReservations(styleSources: Map<string, string>): SpellingReservations {
  const names = new Set<string>();
  let unbounded = false;
  for (const [path, source] of styleSources) {
    try {
      const analysis = stylesheetAnalysis(path, source);
      for (const name of analysis.classNames) names.add(name);
      if (analysis.classReservationsUnbounded) unbounded = true;
    } catch {
      unbounded = true;
    }
  }
  return { names, unbounded };
}

function sameShape(
  left: { property: string; important: boolean }[],
  right: { property: string; important: boolean }[],
): boolean {
  return (
    left.length === right.length &&
    left.every(
      (declaration, index) =>
        declaration.property === right[index].property &&
        declaration.important === right[index].important,
    )
  );
}

/// Aliases accepted for one provisional design system: Tailwind selects the
/// spelling, the compiled declaration shapes match, no authored selector
/// reserves the canonical class, and the compiled canonical output is
/// literal. A declaration that dereferences any custom property keeps its
/// arbitrary spelling until reservation-scanned runtime stability lands
/// with font registration.
export function acceptedCandidateAliases(
  system: DesignSystem,
  probes: string[],
  reservations: SpellingReservations,
): Record<string, string> {
  const aliases: Record<string, string> = {};
  if (reservations.unbounded) return aliases;
  for (const probe of new Set(probes)) {
    const canonical = canonicalCandidate(system, probe);
    // Only a fully named spelling is an idiom improvement; respelling one
    // arbitrary form as another churns byte-exact values (quoting and
    // whitespace) without gaining a name.
    if (canonical === null || canonical.includes("[") || reservations.names.has(canonical)) {
      continue;
    }
    const [compiledProbe, compiledCanonical] = system.candidatesToCss([probe, canonical]);
    if (compiledProbe == null || compiledCanonical == null) continue;
    let probeShape;
    let canonicalShape;
    try {
      probeShape = compiledShape(compiledProbe);
      canonicalShape = compiledShape(compiledCanonical);
    } catch {
      continue;
    }
    if (
      canonicalShape.referencedProperties.length > 0 ||
      probeShape.referencedProperties.length > 0 ||
      !sameShape(probeShape.declarations, canonicalShape.declarations)
    ) {
      continue;
    }
    aliases[probe] = canonical;
  }
  return aliases;
}
