// Entry-group media name resolution and extraction planning.
//
// The native collection pass reports deduplicated generated-variant units
// per package; this module combines every package collection of one entry
// group into a single fixed key-to-name map before any consumer candidate
// is produced. Extraction stays disabled until entry augmentation lands, so
// nothing calls this from the live planning flow yet.

import { collectMediaConditions, mediaProbeKey } from "../native.ts";
import type { LoadedTailwind, Plan, StylesheetEntry } from "../types.ts";

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
 * effective expansion, an existing breakpoint matched by exact value and
 * verified the same way, an adopted definition whose name, normalized
 * meaning, and effective expansion equal what the migration would emit,
 * the readable generated name, the digest name, and finally the
 * arbitrary-variant fallback. Reservation is ownership-blind:
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

  // External reservations block new generated claims only. Adoption of a
  // verified identical definition bypasses them: a second run finds its own
  // first-run names in the scanned candidate corpus, and rejecting adoption
  // for that would break rerun idempotency with duplicate definitions.
  const reserved = new Set<string>(reservedNames);
  for (const token of Object.keys(themeTokens)) {
    if (token.startsWith("breakpoint-")) reserved.add(token.slice("breakpoint-".length));
  }
  const claimed = new Set<string>();
  // The single wrapper key adoption may compare against, or null when the
  // name is absent, opaque, or registered with more than one meaning.
  const adoptableKey = (name: string): string | null => {
    const keys = authoredKeysByName.get(name);
    if (keys === undefined || keys.size !== 1) return null;
    const [key] = keys;
    return key ?? null;
  };
  const taken = (name: string): boolean =>
    claimed.has(name) ||
    reserved.has(name) ||
    authoredKeysByName.has(name) ||
    probes.resolves(name);

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
    // Breakpoint reuse requires the same proof as built-ins: a custom
    // variant can shadow a breakpoint name with unrelated semantics even
    // while the theme token keeps its value.
    if (
      component.breakpoint !== null &&
      probes.expansionMatches(component.breakpoint, component.key)
    ) {
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
      // recognize the first run's definitions. The expansion proof guards
      // against a plugin or dependency registering the same name with
      // different effective semantics behind an identical-looking
      // stylesheet definition.
      if (
        adoptableKey(candidate) === component.key &&
        !claimed.has(candidate) &&
        probes.expansionMatches(candidate, component.key)
      ) {
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
  // Code-point comparison keeps ordering independent of the host locale,
  // matching the byte ordering used on the native side.
  return [...byKey.values()].sort((left, right) =>
    left.key < right.key ? -1 : left.key > right.key ? 1 : 0,
  );
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

/// The probe utility appended to a variant name when compiling against the
/// design system; an arbitrary property compiles under any theme.
const PROBE_UTILITY = "[--tw-probe:1]";

/// The full candidate compiled to probe a variant name. A configured theme
/// prefix must start every candidate, matching the planner's emitted form.
export function probeCandidate(prefix: string | null, name: string): string {
  return `${prefix === null || prefix === "" ? "" : `${prefix}:`}${name}:${PROBE_UTILITY}`;
}

export interface MediaExtraction {
  /** The entry group's fixed key-to-name map, passed to the planner. */
  names: Record<string, string>;
  /** Resolved entries that emit a definition, in sorted key order. */
  generated: { key: string; name: string }[];
}

/**
 * Build the effective-expansion probes against the unaugmented loaded
 * design system. `resolves` proves only existence; `expansionMatches`
 * proves that compiling the name yields exactly one `@media` wrapper whose
 * normalized query equals the key.
 */
export function buildMediaProbes(tailwind: LoadedTailwind): MediaProbes {
  const compiled = new Map<string, string | null>();
  const prefix = tailwind.designSystem.theme.prefix;
  const compile = (name: string): string | null => {
    let css = compiled.get(name);
    if (css === undefined) {
      [css = null] = tailwind.designSystem.candidatesToCss([probeCandidate(prefix, name)]);
      compiled.set(name, css);
    }
    return css;
  };
  return {
    resolves: (name) => compile(name) !== null,
    expansionMatches: (name, key) => {
      const css = compile(name);
      if (css === null) return false;
      const probed: string | null = JSON.parse(mediaProbeKey(css));
      return probed === key;
    },
  };
}

/**
 * Conservative scanned-corpus reservation: every colon-chained token in the
 * package's scanned sources reserves its variant segments, so an inert
 * candidate such as `width-lte-768px:hidden` is never activated with a new
 * meaning. Over-reservation only pushes a generated name to its digest
 * form, and content-identity adoption bypasses these reservations, so
 * reruns stay idempotent.
 */
export function scannedVariantReservations(sources: Iterable<string>): Set<string> {
  const reserved = new Set<string>();
  for (const source of sources) {
    for (const match of source.matchAll(/[A-Za-z][\w-]*(?::[^\s"'`{}]+)+/g)) {
      for (const segment of match[0].split(":").slice(0, -1)) reserved.add(segment);
    }
  }
  return reserved;
}

/**
 * Run collection for every member of one entry group and resolve the fixed
 * map the planner consumes plus the definitions extraction may emit. Name
 * allocation runs once for the complete group, so different keys from
 * different packages can never independently claim one name.
 */
export function planMediaExtraction(options: {
  memberStylesheets: StylesheetEntry[][];
  tailwind: LoadedTailwind;
  scannedSources: Iterable<string>;
}): MediaExtraction {
  const collections = options.memberStylesheets.map(
    (stylesheets): MediaCollection =>
      JSON.parse(
        collectMediaConditions(
          JSON.stringify({
            stylesheets,
            themeTokens: options.tailwind.themeTokens,
            tailwindSources: options.tailwind.graphSources,
          }),
        ),
      ),
  );
  const resolution = resolveMediaNames(
    collections,
    options.tailwind.themeTokens,
    buildMediaProbes(options.tailwind),
    scannedVariantReservations([
      ...options.scannedSources,
      // The entry graph is part of Tailwind's detection corpus too: an
      // `@source inline("width-lte-768px:hidden")` candidate must reserve
      // its segments like any scanned source.
      ...options.tailwind.graphSources.map((graphSource) => graphSource.source),
    ]),
  );
  const names: Record<string, string> = {};
  const generated: { key: string; name: string }[] = [];
  for (const [key, resolved] of resolution.names) {
    names[key] = resolved.name;
    if (resolved.kind === "generated") generated.push({ key, name: resolved.name });
  }
  return { names, generated };
}

/**
 * The generated definitions the final plan actually uses, ordered by the
 * first applied rule that uses each name. A rule counts as applied only
 * when every one of its candidates survived into `plan.candidates`: a
 * retained global rule still applies its utilities to proven consumers,
 * while a blocked rule always contains a dropped candidate, so its
 * occurrences must not fix registration order for a candidate another rule
 * still applies. Registration order controls conflicting utility
 * precedence, so the order follows the applied rules rather than every
 * encountered condition.
 */
export function usedGeneratedDefinitions(
  extraction: MediaExtraction,
  plan: Plan,
): { key: string; name: string }[] {
  const applied = new Set(plan.candidates);
  const byName = new Map(extraction.generated.map((definition) => [definition.name, definition]));
  const ordered: { key: string; name: string }[] = [];
  const seen = new Set<string>();
  for (const rule of plan.rules) {
    if (!rule.candidates.every((candidate) => applied.has(candidate))) continue;
    for (const candidate of rule.candidates) {
      for (const segment of candidate.split(":").slice(0, -1)) {
        const definition = byName.get(segment);
        if (definition !== undefined && !seen.has(segment)) {
          seen.add(segment);
          ordered.push(definition);
        }
      }
    }
  }
  return ordered;
}

/**
 * Append the used definitions to the entry source, each as its own block,
 * matching the planner's moved-block formatting. Definitions already
 * present are recognized through adoption before this runs, so appending
 * never duplicates one.
 */
export function appendMediaDefinitions(
  source: string,
  definitions: { key: string; name: string }[],
): string {
  let output = source;
  for (const { key, name } of definitions) {
    if (!output.endsWith("\n")) output += "\n";
    if (!output.endsWith("\n\n")) output += "\n";
    output += `@custom-variant ${name} {\n  @media ${key} {\n    @slot;\n  }\n}\n`;
  }
  return output;
}
