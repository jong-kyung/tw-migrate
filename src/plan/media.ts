// Entry-group media name resolution.
//
// The native collection pass reports deduplicated generated-variant units
// per package; this module combines every package collection of one entry
// group into a single fixed key-to-name map before any consumer candidate
// is produced. Extraction stays disabled until entry augmentation lands, so
// nothing calls this from the live planning flow yet.

/// The reserved namespace for digest fallback names.
const GENERATED_NAMESPACE = "twm-media";

export interface CollectedMediaComponent {
  key: string;
  whole: boolean;
  readableName: string | null;
  digest: string;
  builtin: string | null;
  breakpoint: string | null;
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
  components: CollectedMediaComponent[];
  authoredVariants: AuthoredMediaVariant[];
}

export interface MediaProbes {
  /// True when the unaugmented loaded design system already resolves the
  /// variant name, wherever its owner lives: authored stylesheets, plugins,
  /// or dependency-owned imports.
  resolves: (variantName: string) => boolean;
  /// True when the design system's effective compiled expansion of the name
  /// is exactly the `@media` wrapper for this key. Built-in reuse requires
  /// this proof, because a project may redefine a built-in name such as
  /// `dark` with selector semantics.
  expansionMatches: (variantName: string, conditionKey: string) => boolean;
}

export interface ResolvedMediaName {
  name: string;
  /// How the name resolved: a verified built-in, an existing breakpoint, an
  /// adopted identical definition, or a generated definition this migration
  /// emits.
  kind: "builtin" | "breakpoint" | "adopted" | "generated";
  whole: boolean;
  cssPath: string;
  order: number;
}

export interface MediaNameResolution {
  /// Key to resolved name, in sorted key order. Only `generated` entries
  /// emit a definition.
  names: Map<string, ResolvedMediaName>;
  /// Keys whose names could not escape existing owners; these conditions
  /// keep the arbitrary-variant fallback.
  fallbacks: Set<string>;
}

/**
 * Resolve one fixed name per key across a complete entry group.
 *
 * Resolution follows the RFC order: a built-in candidate with a verified
 * effective expansion, an existing breakpoint matched by exact value, an
 * adopted definition whose name and normalized meaning equal what the
 * migration would emit, the readable generated name, the digest name, and
 * finally the arbitrary-variant fallback. Reservation is ownership-blind:
 * authored names with any different meaning, active theme names, explicit
 * reserved names such as inert scanned candidates, and any name the design
 * system resolves all block a generated name.
 */
export function resolveMediaNames(
  collections: MediaCollection[],
  themeTokens: Record<string, string>,
  probes: MediaProbes,
  reservedNames: Iterable<string> = [],
): MediaNameResolution {
  const components = mergeComponents(collections);
  const authoredKeysByName = mergeAuthoredVariants(collections);

  const claimed = new Set<string>(reservedNames);
  for (const token of Object.keys(themeTokens)) {
    if (token.startsWith("breakpoint-")) claimed.add(token.slice("breakpoint-".length));
  }
  // The single wrapper key adoption may compare against, or null when the
  // name is absent, opaque, or registered with more than one meaning.
  const adoptableKey = (name: string): string | null => {
    const keys = authoredKeysByName.get(name);
    if (keys === undefined || keys.size !== 1) return null;
    const [key] = keys;
    return key ?? null;
  };
  const taken = (name: string): boolean =>
    claimed.has(name) || authoredKeysByName.has(name) || probes.resolves(name);

  const names = new Map<string, ResolvedMediaName>();
  const fallbacks = new Set<string>();
  for (const component of components) {
    const resolved = (name: string, kind: ResolvedMediaName["kind"]): void => {
      claimed.add(name);
      names.set(component.key, {
        name,
        kind,
        whole: component.whole,
        cssPath: component.cssPath,
        order: component.order,
      });
    };

    if (component.builtin !== null && probes.expansionMatches(component.builtin, component.key)) {
      resolved(component.builtin, "builtin");
      continue;
    }
    if (component.breakpoint !== null) {
      resolved(component.breakpoint, "breakpoint");
      continue;
    }

    const digestName = `${GENERATED_NAMESPACE}-${component.digest}`;
    const candidates =
      component.readableName === null ? [digestName] : [component.readableName, digestName];
    let done = false;
    for (const candidate of candidates) {
      // Adoption is decided by content rather than authorship: an existing
      // definition whose name and normalized meaning equal what the
      // migration would emit is used as-is, which also makes a second run
      // recognize the first run's definitions.
      if (adoptableKey(candidate) === component.key && !claimed.has(candidate)) {
        resolved(candidate, "adopted");
        done = true;
        break;
      }
      if (!taken(candidate)) {
        resolved(candidate, "generated");
        done = true;
        break;
      }
    }
    if (!done) fallbacks.add(component.key);
  }
  return { names, fallbacks };
}

/// Deduplicate components by key across package collections, keeping the
/// first occurrence in collection order, then sort by key so resolution is
/// independent of discovery order.
function mergeComponents(collections: MediaCollection[]): CollectedMediaComponent[] {
  const byKey = new Map<string, CollectedMediaComponent>();
  for (const collection of collections) {
    for (const component of collection.components) {
      if (!byKey.has(component.key)) byKey.set(component.key, component);
    }
  }
  return [...byKey.values()].sort((left, right) => left.key.localeCompare(right.key));
}

/// The set of wrapper keys registered under each authored name. Packages
/// sharing one entry graph report the same rows, so identity deduplication
/// is safe; a name mapping to anything other than exactly one wrapper key
/// can never be adopted.
function mergeAuthoredVariants(collections: MediaCollection[]): Map<string, Set<string | null>> {
  const byName = new Map<string, Set<string | null>>();
  for (const collection of collections) {
    for (const variant of collection.authoredVariants) {
      const keys = byName.get(variant.name) ?? new Set<string | null>();
      keys.add(variant.mediaQueryKey);
      byName.set(variant.name, keys);
    }
  }
  return byName;
}
