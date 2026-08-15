// Per-candidate canonicalization against one loaded Tailwind design system.
// The bridge owns only the spelling lookup; declaration-shape and
// runtime-stability proofs stay with the caller. Results are cached per
// design-system instance because provisional entries (generated media
// variants, proposed font tokens) change the answer.

import { dirname } from "node:path";

import { compiledShape, cssDirectives, stylesheetAnalysis } from "../native.ts";
import { stylesheetReferenceTargets } from "../util/shared.ts";
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
  /// Nonempty static prefixes of dynamic class construction in source
  /// files; a canonical spelling the runtime value could complete is
  /// reserved.
  prefixes: Set<string>;
  /// True when any snapshot's class match set cannot be bounded (an
  /// unparseable sheet, an interpolated class, or an attribute matcher
  /// beyond word equality); every alias is then rejected.
  unbounded: boolean;
}

/// Authored-selector reservations over the complete stylesheet snapshot,
/// including the entry's loaded import graph. Migration stylesheets never
/// join the Tailwind scan corpus, so a retained legacy rule such as
/// `.mr-auto` would silently activate on a migrated element unless its
/// spelling is reserved here.
export function spellingReservations(
  styleSources: Map<string, string>,
  /// Entry-graph sheets under synthetic keys. Their import graph was
  /// already resolved by the Tailwind loader, so only their selectors
  /// participate; unresolved references never fire for them.
  resolvedExtras: Iterable<readonly [string, string]>,
  /// `importerDirectory\0specifier` keys the Tailwind loader already
  /// resolved into the extras, so the entry graph's own package imports do
  /// not read as opaque. Keys carry the importer because the same
  /// specifier text can resolve differently from another location.
  resolvedImports: Set<string> = new Set(),
): SpellingReservations {
  const names = new Set<string>();
  let unbounded = false;
  const scan = (path: string, source: string, entryGraph: boolean) => {
    try {
      const analysis = stylesheetAnalysis(path, source);
      for (const name of analysis.classNames) names.add(name);
      if (analysis.classReservationsUnbounded) unbounded = true;
      if (entryGraph) {
        // A plugin or legacy config module can emit any selector into the
        // built output without it appearing in scanned CSS, and a
        // `@source not` exclusion can keep Tailwind from generating a
        // canonical spelling the migrated source would then rely on.
        if (entryDirectiveHazard(source)) unbounded = true;
        // The loader already resolved the entry graph's imports.
        return;
      }
      // An interpolated reference can load a sheet this scan never sees.
      if (analysis.unverifiable) unbounded = true;
      for (const reference of analysis.references) {
        // The Tailwind package emits utilities, never authored selectors,
        // and Sass built-in modules define functions without loading any
        // authored sheet.
        if (reference === "tailwindcss" || reference.startsWith("tailwindcss/")) continue;
        if (reference.startsWith("sass:")) continue;
        if (resolvedImports.has(`${dirname(path)}\0${reference}`) || styleSources.has(reference))
          continue;
        // A reference outside the snapshot loads selectors this scan
        // cannot see: a package or remote import, or a sheet discovery
        // never captured.
        if (stylesheetReferenceTargets(path, reference, styleSources).length === 0) {
          unbounded = true;
        }
      }
    } catch {
      unbounded = true;
    }
  };
  for (const [path, source] of styleSources) scan(path, source, false);
  for (const [path, source] of resolvedExtras) scan(path, source, true);
  return { names, prefixes: new Set(), unbounded };
}

/// True when an entry-graph sheet declares a directive whose effect on the
/// built output this scan cannot see: `@plugin` and `@config` run
/// JavaScript that may emit arbitrary selectors, and a `@source not`
/// prelude (or one that cannot be read) excludes sources or candidates
/// from generation.
function entryDirectiveHazard(source: string): boolean {
  const directives = cssDirectives(source);
  if (directives === null) return true;
  return directives.some((directive) => {
    if (directive === null || typeof directive !== "object" || !("kind" in directive)) {
      return false;
    }
    // A source(...) import modifier disables or restricts automatic
    // scanning, so a canonical spelling may never reach generation even
    // though the arbitrary form was safelisted.
    if (directive.kind === "import") {
      return (
        ("source" in directive && directive.source !== null) ||
        ("sourceUnreadable" in directive && directive.sourceUnreadable === true)
      );
    }
    if (directive.kind === "source") {
      return (
        ("not" in directive && directive.not === true) ||
        ("unreadable" in directive && directive.unreadable === true)
      );
    }
    return (
      directive.kind === "other" &&
      "name" in directive &&
      (directive.name === "plugin" || directive.name === "config")
    );
  });
}

export function reservedSpelling(reservations: SpellingReservations, spelling: string): boolean {
  if (reservations.names.has(spelling)) return true;
  for (const prefix of reservations.prefixes) {
    if (spelling.startsWith(prefix)) return true;
  }
  return false;
}

/// The utility segment of one complete candidate: everything after the
/// last top-level variant separator, bracket-aware so arbitrary variants
/// keep their inner colons.
function utilitySegment(candidate: string): string {
  let depth = 0;
  let start = 0;
  for (let index = 0; index < candidate.length; index += 1) {
    const character = candidate[index];
    if (character === "[" || character === "(") depth += 1;
    else if (character === "]" || character === ")") depth -= 1;
    else if (character === ":" && depth === 0) start = index + 1;
  }
  return candidate.slice(start);
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
    // Only a fully named utility is an idiom improvement; respelling one
    // arbitrary form as another churns byte-exact values without gaining a
    // name, while brackets inside a preserved variant chain stay welcome.
    if (
      canonical === null ||
      utilitySegment(canonical).includes("[") ||
      reservedSpelling(reservations, canonical)
    ) {
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
