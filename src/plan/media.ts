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
 * theme names, and any name the `probe` resolves against the unaugmented
 * design system (plugin and dependency variants included) all block a
 * generated name. Probing is bounded: preferred name, then the
 * digest-suffixed form, then one name in the reserved generated namespace,
 * and finally the arbitrary-variant fallback.
 */
export function allocateMediaNames(
  collections: MediaCollection[],
  themeTokens: Record<string, string>,
  probe: (variantName: string) => boolean,
): MediaNameAllocation {
  const conditions = mergeConditions(collections);
  const authoredByName = mergeAuthoredVariants(collections);

  const claimed = new Set<string>();
  for (const name of authoredByName.keys()) claimed.add(name);
  for (const token of Object.keys(themeTokens)) {
    if (token.startsWith("breakpoint-")) claimed.add(token.slice("breakpoint-".length));
  }
  const taken = (name: string): boolean => claimed.has(name) || probe(name);

  const names = new Map<string, AllocatedMediaName>();
  const fallbacks = new Set<string>();
  for (const condition of conditions) {
    const authored = authoredByName.get(condition.preferredName) ?? [];
    const [single] = authored;
    if (
      authored.length === 1 &&
      single !== undefined &&
      single.mediaQueryKey === condition.key &&
      probe(single.name)
    ) {
      names.set(condition.key, {
        name: single.name,
        kind: condition.kind,
        reused: true,
        cssPath: condition.cssPath,
        order: condition.order,
      });
      continue;
    }

    const attempts = [
      condition.preferredName,
      digestSuffixed(condition.preferredName, condition.digest),
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
/// the same authored variants; identical (name, path, definition) rows
/// collapse, while same-name rows from different sources stay separate so
/// duplicate registrations count as collisions and are never reused.
function mergeAuthoredVariants(
  collections: MediaCollection[],
): Map<string, AuthoredMediaVariant[]> {
  const byName = new Map<string, AuthoredMediaVariant[]>();
  const seen = new Set<string>();
  for (const collection of collections) {
    for (const variant of collection.authoredVariants) {
      const identity = `${variant.name}\0${variant.path}\0${variant.definition}`;
      if (seen.has(identity)) continue;
      seen.add(identity);
      const rows = byName.get(variant.name) ?? [];
      rows.push(variant);
      byName.set(variant.name, rows);
    }
  }
  return byName;
}

/// Mirrors digest_suffixed_name in crates/tw_migrate/src/media.rs: keep the
/// readable prefix inside the fixed length limit and append the digest.
function digestSuffixed(name: string, digest: string): string {
  const budget = MAX_NAME_LENGTH - digest.length - 1;
  const prefix = (name.length > budget ? name.slice(0, budget) : name).replace(/-+$/, "");
  return `${prefix}-${digest}`;
}
