// Font theme-token naming and reuse for canonicalization. The planner
// supplies parsed family stacks; this module owns the deterministic name
// derivation and the targeted lookup of existing `--font-*` tokens, while
// provisional registration and safety proofs stay with the caller.

import { compiledShape, fontFamilyStack } from "../native.ts";
import { isIntegrityError } from "../util/shared.ts";
import { canonicalCandidate, reservedSpelling, utilitySegment } from "./canonicalize.ts";
import type { SpellingReservations } from "./canonicalize.ts";
import type { DesignSystem, FontFamilyProbe } from "../types.ts";

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

/// The probe's candidate with its utility segment replaced by a named
/// font utility, preserving the variant chain, Tailwind prefix, and
/// important modifier.
function aliasedCandidate(candidate: string, utility: string): string {
  const segment = utilitySegment(candidate);
  const important = segment.endsWith("!") ? "!" : "";
  return candidate.slice(0, candidate.length - segment.length) + utility + important;
}

/// One accepted font registration outcome for a planning group.
export interface FontRegistration {
  /// Probe candidate to aliased spelling, merged into candidateAliases.
  aliases: Record<string, string>;
  /// Newly generated token names (without --font-) to stack values; empty
  /// when every alias reused an existing token.
  tokens: Record<string, string>;
  /// Probe candidates that attempted registration and produced no alias:
  /// exhausted allocation, a provisional entry that would not load, or a
  /// semantic rejection. Rules these retain warn as registration
  /// failures.
  failures: Set<string>;
}

/// A generated `@theme` block after existing entry content, following the
/// media-definition append conventions.
export function appendFontTheme(css: string, tokens: Record<string, string>): string {
  const names = Object.keys(tokens).sort();
  if (names.length === 0) return css;
  let output = css;
  if (!output.endsWith("\n")) output += "\n";
  if (!output.endsWith("\n\n")) output += "\n";
  output += `@theme {\n${names.map((name) => `  --font-${name}: ${tokens[name]};\n`).join("")}}\n`;
  return output;
}

/// Theme-backed shape acceptance for one probe/alias pair: identical
/// effective declarations, a literal probe side, and canonical-side
/// custom-property references that no authored surface outside the entry
/// graph declares, registers, reads, or mentions. Reads over-reserve, but
/// that only costs a cosmetic reuse.
function fontShapesAccept(
  system: DesignSystem,
  probeCandidate: string,
  aliased: string,
  reservations: SpellingReservations,
  /// For targeted reuse: the compiled utility must dereference this
  /// token's runtime property, or a custom utility owning the spelling
  /// could substitute an unrelated value without a canonicalization
  /// proof.
  backingToken?: string,
): boolean {
  const [compiledProbe, compiledAliased] = system.candidatesToCss([probeCandidate, aliased]);
  if (compiledProbe == null || compiledAliased == null) return false;
  let probeShape;
  let aliasedShape;
  try {
    probeShape = compiledShape(compiledProbe);
    aliasedShape = compiledShape(compiledAliased);
  } catch {
    return false;
  }
  if (probeShape.referencedProperties.length > 0) return false;
  if (aliasedShape.referencedProperties.some((property) => reservations.properties.has(property))) {
    return false;
  }
  if (backingToken !== undefined) {
    // The compiled utility must be a bare dereference of the matched
    // token: a custom utility composing the token with other families,
    // or substituting an unrelated value, keeps the arbitrary spelling.
    const bare = aliasedShape.declarations.some(
      (declaration) =>
        declaration.property === "font-family" &&
        /^var\(--[\w-]+\)$/.test(declaration.value) &&
        (declaration.value === `var(--${backingToken})` ||
          declaration.value.endsWith(`-${backingToken})`)),
    );
    if (!bare) return false;
  }
  return (
    probeShape.declarations.length === aliasedShape.declarations.length &&
    probeShape.declarations.every(
      (declaration, index) =>
        declaration.property === aliasedShape.declarations[index].property &&
        declaration.important === aliasedShape.declarations[index].important,
    )
  );
}

export interface FontRegistrationOptions {
  /// The media-provisional pre-font design system.
  system: DesignSystem;
  /// The provisional entry source the system was loaded from.
  entryCss: string;
  loadWith: (css: string) => Promise<DesignSystem>;
  themeTokens: Record<string, string>;
  probes: FontFamilyProbe[];
  /// Distinct candidates whose compiled output must not change when a
  /// token registers. This approximates the effective scan corpus with
  /// the migration's own candidates; full corpus enumeration would need
  /// Tailwind's scanner.
  corpus: string[];
  reservations: SpellingReservations;
  /// Probe candidates the plain canonicalization pass already aliased.
  existingAliases: Record<string, string>;
  /// False for an unwritable entry: existing tokens may still be reused
  /// because reuse needs no entry edit, but no new token registers.
  generate: boolean;
  /// Utility segments of class-like tokens already present in group
  /// sources; allocation never adopts one.
  mentionedSegments: Set<string>;
  /// Theme tokens loaded in Tailwind reference mode, which never emit
  /// their custom properties at runtime.
  referenceTokens: Set<string>;
  /// The run-wide allocation registry (token name to stack value):
  /// emitted @theme variables are global at runtime, so two entry groups
  /// deriving one name from different stacks must not share it, while
  /// the same stack shares the spelling with per-entry definitions. A
  /// null value is an opaque owner no probe stack may match.
  globalAllocations: Map<string, string | null>;
}

/// Font aliases for one planning group: reuse an existing matching token
/// when a safe one exists, otherwise allocate a fresh token name, load
/// every proposed token provisionally, and accept only aliases the
/// font-provisional system canonicalizes to exactly the allocated
/// spelling with matching shapes, stable references, and an unchanged
/// corpus.
export async function registerFontTokens(
  options: FontRegistrationOptions,
): Promise<FontRegistration> {
  const { system, reservations } = options;
  if (reservations.unbounded || reservations.propertiesUnbounded) {
    return { aliases: {}, tokens: {}, failures: new Set() };
  }
  const prefix = system.theme.prefix;

  const aliases: Record<string, string> = {};
  const failures = new Set<string>();
  const seen = new Set<string>();
  /// Repeated occurrences of one normalized stack share one allocation.
  const allocatedByStack = new Map<string, string>();
  const proposals: { probe: FontFamilyProbe; name: string }[] = [];
  const claimed = new Set<string>();

  for (const probe of options.probes) {
    if (seen.has(probe.candidate)) continue;
    seen.add(probe.candidate);
    if (probe.firstFamily.kind !== "name") continue;
    if (probe.candidate in options.existingAliases) continue;

    // Targeted reuse: the lexicographically first existing token whose
    // stack matches and whose compiled utility passes every proof.
    let reused = false;
    for (const token of matchingFontTokens(
      options.themeTokens,
      probe.value,
      options.referenceTokens,
    )) {
      // The reused spelling obeys the same reservation rules as ordinary
      // canonical aliases: the token name, its aliased candidate, and the
      // prefixed-candidate forms must all stay unreserved.
      const aliased = aliasedCandidate(probe.candidate, token);
      if (reservedSpelling(reservations, token) || reservedSpelling(reservations, aliased)) {
        continue;
      }
      // A reused token also joins the run-wide registry: another group
      // must not generate the same global property for a different stack.
      const reuseName = token.replace(/^font-/, "");
      const owner = options.globalAllocations.get(reuseName);
      if (owner !== undefined && (owner === null || owner !== probe.value)) continue;
      // The probe candidate carries the entry prefix inside its chain, so
      // the segment swap keeps prefix, variants, and importance.
      if (fontShapesAccept(system, probe.candidate, aliased, reservations, token)) {
        aliases[probe.candidate] = aliased;
        options.globalAllocations.set(reuseName, probe.value);
        reused = true;
        break;
      }
    }
    if (reused) continue;
    if (!options.generate) continue;

    const stackKey = probe.value;
    const shared = allocatedByStack.get(stackKey);
    if (shared !== undefined) {
      proposals.push({ probe, name: shared });
      continue;
    }
    const base = fontTokenName(probe.firstFamily.name);
    let allocated = false;
    // The RFC's bounded sequence: the base spelling, then numeric
    // suffixes, at most 100 attempts before registration fails.
    for (let attempt = 1; attempt <= 100; attempt += 1) {
      const name = attempt === 1 ? base : `${base}-${attempt}`;
      if (claimed.has(name)) continue;
      // A prefixed entry dereferences the token through the prefixed
      // runtime property, so both spellings must be free; otherwise the
      // shape proof rejects terminally where a suffix would have worked.
      if (reservations.properties.has(`--font-${name}`)) continue;
      if (prefix !== null && reservations.properties.has(`--${prefix}-font-${name}`)) continue;
      if (reservedSpelling(reservations, `font-${name}`)) continue;
      if (options.mentionedSegments.has(`font-${name}`)) continue;
      const global = options.globalAllocations.get(name);
      if (global !== undefined && (global === null || global !== probe.value)) continue;
      // A spelling the pre-font system already compiles is owned by an
      // existing utility, such as font-bold.
      const compiled = prefix !== null ? `${prefix}:font-${name}` : `font-${name}`;
      if (system.candidatesToCss([compiled])[0] !== null) continue;
      claimed.add(name);
      allocatedByStack.set(stackKey, name);
      proposals.push({ probe, name });
      allocated = true;
      break;
    }
    if (!allocated) failures.add(probe.candidate);
  }
  if (proposals.length === 0) return { aliases, tokens: {}, failures };

  const tokens: Record<string, string> = {};
  for (const { probe, name } of proposals) tokens[name] = probe.value;
  let fontSystem: DesignSystem;
  try {
    fontSystem = await options.loadWith(appendFontTheme(options.entryCss, tokens));
  } catch (error) {
    // Source drift during the reload is fatal like every other integrity
    // failure; only an ordinary load failure registers nothing.
    if (isIntegrityError(error)) throw error;
    for (const { probe } of proposals) failures.add(probe.candidate);
    return { aliases, tokens: {}, failures };
  }

  // The caller passes distinct candidates already.
  const before = system.candidatesToCss(options.corpus);
  const after = fontSystem.candidatesToCss(options.corpus);
  const corpusChanged = options.corpus.some((candidate, index) => before[index] !== after[index]);

  const accepted: Record<string, string> = {};
  for (const { probe, name } of proposals) {
    const canonical = corpusChanged ? null : canonicalCandidate(fontSystem, probe.candidate);
    const expected = aliasedCandidate(probe.candidate, `font-${name}`);
    // Semantic rejection is terminal: another suffix cannot change the
    // candidate's behavior.
    if (
      canonical !== expected ||
      !fontShapesAccept(fontSystem, probe.candidate, expected, reservations)
    ) {
      failures.add(probe.candidate);
      continue;
    }
    aliases[probe.candidate] = expected;
    accepted[name] = probe.value;
  }
  return { aliases, tokens: accepted, failures };
}

/// Existing `--font-*` theme tokens whose parsed stack equals the probe's
/// normalized stack, as utility candidate names sorted lexicographically.
/// The comparison runs both values through the planner's stack parser, so
/// authored spacing and quoting differences cannot defeat reuse, and an
/// unreadable token value never matches.
export function matchingFontTokens(
  themeTokens: Record<string, string>,
  normalizedStack: string,
  referenceTokens: Set<string> = new Set(),
): string[] {
  const matches: string[] = [];
  for (const [token, value] of Object.entries(themeTokens)) {
    if (!token.startsWith("font-")) continue;
    // A reference-mode token never emits its custom property at runtime,
    // so a utility dereferencing it would resolve to nothing.
    if (referenceTokens.has(token)) continue;
    const parsed = fontFamilyStack(value);
    if (parsed !== null && parsed.value === normalizedStack) {
      matches.push(token);
    }
  }
  matches.sort();
  return matches;
}
