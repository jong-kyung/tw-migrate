// Entry-group media definition name allocation.
//
// The native collection pass reports unmatched media conditions and authored
// custom-variant reservations per package; this allocator combines every
// package collection of one entry group into a single fixed
// condition-key-to-name map before any consumer candidate is produced.
// Extraction stays disabled until entry augmentation lands, so nothing calls
// this from the live planning flow yet.

/// Mirrors MAX_NAME_LENGTH in crates/tw_migrate/src/media.rs.
const MAX_NAME_LENGTH = 48;

/// The reserved namespace probed when suffixing cannot escape an existing
/// functional variant namespace.
const GENERATED_NAMESPACE = "twm-media";

export interface CollectedMediaCondition {
  key: string;
  kind: "breakpoint" | "customVariant";
  preferredName: string;
  digest: string;
  cssPath: string;
  order: number;
}

export interface AuthoredMediaVariant {
  name: string;
  definition: string;
  mediaQueryKey: string | null;
  path: string;
}

export interface MediaCollection {
  conditions: CollectedMediaCondition[];
  authoredVariants: AuthoredMediaVariant[];
  breakpointUnit: string | null;
}

export interface MediaProbes {
  /// True when the unaugmented loaded design system already resolves the
  /// variant name, wherever its owner lives: authored stylesheets, plugins,
  /// or dependency-owned imports.
  resolves: (variantName: string) => boolean;
  /// True when the design system's effective compiled expansion of the name
  /// is exactly the authored media wrapper for this condition key. Reuse
  /// requires this proof; a bare existence probe cannot rule out a competing
  /// plugin or dependency registration under the same name.
  expansionMatches: (variantName: string, conditionKey: string) => boolean;
}

export interface AllocatedMediaName {
  name: string;
  kind: "breakpoint" | "customVariant";
  /// True when an authored definition with a proven matching meaning is
  /// reused, so the migration emits no definition for this condition.
  reused: boolean;
  cssPath: string;
  order: number;
}

export interface MediaNameAllocation {
  /// Condition key to allocated name, in sorted key order.
  names: Map<string, AllocatedMediaName>;
  /// Condition keys whose names could not escape existing owners; these
  /// conditions keep the arbitrary-variant fallback.
  fallbacks: Set<string>;
}

/**
 * Allocate one fixed name per condition key across a complete entry group.
 *
 * Reservation is ownership-blind: authored custom-variant names, active
 * theme names, explicitly reserved names such as inert scanned candidates
 * and non-theme breakpoint custom properties, and any name the design
 * system resolves all block a generated name. Probing is bounded: preferred
 * name, then the digest-suffixed form, then one name in the reserved
 * generated namespace, and finally the arbitrary-variant fallback.
 */
export function allocateMediaNames(
  collections: MediaCollection[],
  themeTokens: Record<string, string>,
  probes: MediaProbes,
  reservedNames: Iterable<string> = [],
): MediaNameAllocation {
  const conditions = mergeConditions(collections);
  const authoredByName = mergeAuthoredVariants(collections);
  const namesByMeaning = indexAuthoredMeanings(authoredByName);

  const claimed = new Set<string>(reservedNames);
  for (const name of authoredByName.keys()) claimed.add(name);
  for (const token of Object.keys(themeTokens)) {
    if (token.startsWith("breakpoint-")) claimed.add(token.slice("breakpoint-".length));
  }
  const taken = (name: string): boolean => claimed.has(name) || probes.resolves(name);

  const names = new Map<string, AllocatedMediaName>();
  const fallbacks = new Set<string>();
  for (const condition of conditions) {
    const reusable = reusableAuthoredName(condition.key, namesByMeaning, authoredByName, probes);
    if (reusable !== undefined) {
      names.set(condition.key, {
        name: reusable,
        kind: condition.kind,
        reused: true,
        cssPath: condition.cssPath,
        order: condition.order,
      });
      continue;
    }

    const preferred =
      condition.preferredName.length > MAX_NAME_LENGTH
        ? digestSuffixed(condition.preferredName, condition.digest)
        : condition.preferredName;
    const attempts = [
      preferred,
      digestSuffixed(preferred, condition.digest),
      `${GENERATED_NAMESPACE}-${condition.digest}`,
    ];
    const chosen = attempts.find((name) => !taken(name));
    if (chosen === undefined) {
      fallbacks.add(condition.key);
      continue;
    }
    claimed.add(chosen);
    names.set(condition.key, {
      name: chosen,
      kind: condition.kind,
      reused: false,
      cssPath: condition.cssPath,
      order: condition.order,
    });
  }
  return { names, fallbacks };
}

/// The one authored name whose meaning provably equals the condition key, or
/// `undefined` when reuse is unsafe. Reuse requires exactly one authored
/// name for the meaning, exactly one registration of that name, and an
/// effective-expansion proof against the loaded design system, so duplicate
/// and opaque registrations count as collisions.
function reusableAuthoredName(
  conditionKey: string,
  namesByMeaning: Map<string, Set<string>>,
  authoredByName: Map<string, AuthoredMediaVariant[]>,
  probes: MediaProbes,
): string | undefined {
  const meaningNames = namesByMeaning.get(conditionKey);
  if (meaningNames === undefined || meaningNames.size !== 1) return undefined;
  const [name] = meaningNames;
  if (name === undefined || (authoredByName.get(name) ?? []).length !== 1) return undefined;
  return probes.expansionMatches(name, conditionKey) ? name : undefined;
}

/// Deduplicate conditions by key across package collections, keeping the
/// first occurrence in collection order, then sort by key so allocation is
/// independent of discovery order.
function mergeConditions(collections: MediaCollection[]): CollectedMediaCondition[] {
  const byKey = new Map<string, CollectedMediaCondition>();
  for (const collection of collections) {
    for (const condition of collection.conditions) {
      if (!byKey.has(condition.key)) byKey.set(condition.key, condition);
    }
  }
  return [...byKey.values()].sort((left, right) => left.key.localeCompare(right.key));
}

/// Group authored variants by name. Packages sharing one entry graph report
/// the same authored rows, so identical rows deduplicate across collections
/// by taking each row's highest per-collection multiplicity; duplicate
/// declarations inside one source keep their multiplicity and therefore
/// count as collisions.
function mergeAuthoredVariants(
  collections: MediaCollection[],
): Map<string, AuthoredMediaVariant[]> {
  const byIdentity = new Map<string, { variant: AuthoredMediaVariant; count: number }>();
  for (const collection of collections) {
    const local = new Map<string, number>();
    for (const variant of collection.authoredVariants) {
      const identity = `${variant.name}\0${variant.path}\0${variant.definition}`;
      local.set(identity, (local.get(identity) ?? 0) + 1);
      if (!byIdentity.has(identity)) byIdentity.set(identity, { variant, count: 0 });
    }
    for (const [identity, count] of local) {
      const entry = byIdentity.get(identity);
      if (entry !== undefined) entry.count = Math.max(entry.count, count);
    }
  }
  const byName = new Map<string, AuthoredMediaVariant[]>();
  for (const { variant, count } of byIdentity.values()) {
    const rows = byName.get(variant.name) ?? [];
    for (let occurrence = 0; occurrence < count; occurrence += 1) rows.push(variant);
    byName.set(variant.name, rows);
  }
  return byName;
}

/// Index authored media-wrapper definitions by their normalized condition
/// key, so a matching definition is found under any authored name, not only
/// under the generated preferred name.
function indexAuthoredMeanings(
  authoredByName: Map<string, AuthoredMediaVariant[]>,
): Map<string, Set<string>> {
  const byMeaning = new Map<string, Set<string>>();
  for (const [name, rows] of authoredByName) {
    for (const row of rows) {
      if (row.mediaQueryKey === null) continue;
      const names = byMeaning.get(row.mediaQueryKey) ?? new Set<string>();
      names.add(name);
      byMeaning.set(row.mediaQueryKey, names);
    }
  }
  return byMeaning;
}

/// Mirrors digest_suffixed_name in crates/tw_migrate/src/media.rs: keep the
/// readable prefix inside the fixed length limit and append the digest.
function digestSuffixed(name: string, digest: string): string {
  const budget = MAX_NAME_LENGTH - digest.length - 1;
  const prefix = (name.length > budget ? name.slice(0, budget) : name).replace(/-+$/, "");
  return `${prefix}-${digest}`;
}
