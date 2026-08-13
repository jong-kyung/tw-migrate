# RFC: Tailwind Candidate Canonicalization and Font Theme Registration

## Status

Proposed

## Summary

This RFC refines the declaration-conversion policy in [CSS-to-Tailwind Migration CLI](./css-to-tailwind-migration-cli.md).

`tw-migrate` currently emits valid but non-idiomatic arbitrary utilities when Tailwind already provides a named utility. Examples include `mr-[auto]` for `margin-right: auto`, `max-w-[100%]` for `max-width: 100%`, and `[font-family:"Open_Sans",_sans-serif]` for a custom font stack.

This RFC moves final candidate selection into the target project's Tailwind v4 design system. The JavaScript orchestration layer canonicalizes planner candidates with `canonicalizeCandidates()`, then asks the native planner to produce the source edits again with the canonical names. Values without a named equivalent keep their arbitrary utility.

For an unregistered explicit font family, the migration appends a Tailwind v4 `@theme` token to the owning Tailwind entry and emits the generated `font-*` utility. The migration retains the affected rule when it cannot edit the entry safely.

## Motivation

The Rust planner currently owns both declaration analysis and candidate spelling. `crates/tw_migrate/src/utilities.rs` contains a partial table of static utilities and theme namespaces. Its generic fallback produces an arbitrary value or arbitrary property. This preserves CSS values, but it duplicates part of Tailwind's candidate-selection logic and misses built-in names that Tailwind already knows.

The target project's loaded design system can canonicalize these candidates using its installed Tailwind version, prefix, theme, and plugins. With the repository's current Tailwind installation, the API produces these results before runtime-stability filtering:

```text
mr-[auto]          -> mr-auto
max-w-[100%]       -> max-w-full
p-[1rem]           -> p-4
hover:mr-[auto]    -> hover:mr-auto
mr-[auto]!         -> mr-auto!
p-[13px]           -> p-[13px]
```

The same API resolves an arbitrary font family to a named theme utility after the entry defines a matching `--font-*` token.

## Goals

1. Prefer the target Tailwind installation's canonical utility for every generated single utility.
2. Preserve the exact declared value when Tailwind has no runtime-stable named equivalent.
3. Respect Tailwind prefixes, variants, important modifiers, custom theme values, and the installed v4 minor version.
4. Register explicit non-generic font families as Tailwind v4 font theme tokens.
5. Reuse an existing font token when its value matches exactly.
6. Generate deterministic, collision-free font token names.
7. Retain a font rule when the owning Tailwind entry cannot be modified safely.
8. Keep preview, write, batch, workspace, and rerun behavior deterministic.

## Non-Goals

1. Combining several declarations into a compound utility such as `truncate`.
2. Solving a minimum exact-cover problem across multiple utilities.
3. Reusing unrelated project `@utility` or plugin classes because their generated declarations happen to match.
4. Creating theme tokens for colors, spacing, radii, shadows, or other non-font values.
5. Approximating a value with the nearest theme token.
6. Changing font loading, moving external font imports, or generating `@font-face` rules.
7. Replacing runtime-dependent values such as `font-family: var(--font-body)` with a generated theme token.

Compound utility selection remains deferred because indexing the current Tailwind class list generates about 14 MB of CSS for 23,286 candidates per entry. Single-candidate canonicalization fixes the reported arbitrary-value problem without that startup and memory cost.

## Terminology

- **Planner candidate**: the exact utility produced by the native planner before Tailwind canonicalization, such as `mr-[auto]`.
- **Canonical candidate**: the utility returned by the target design system, such as `mr-auto`.
- **Candidate alias**: an internal mapping from one complete planner candidate to one canonical candidate.
- **Candidate probe**: a complete candidate considered for a rewrite site, including prefix and variants, even when the candidate cannot yet fit that site's source representation.
- **Generated font token**: a `--font-*` variable appended in a new `@theme` block by this migration.
- **Runtime-stable alias**: a canonical candidate whose compiled value cannot change through an authored custom-property override in the migration snapshot.

## Current Flow

A package-entry group currently follows this path:

1. TypeScript loads the target Tailwind design system.
2. TypeScript sends stylesheets, theme tokens, source files, and the Tailwind entry to the native batch planner.
3. Rust converts declarations into candidates and plans source edits.
4. TypeScript calls `candidatesToCss()` to reject candidates that Tailwind cannot compile.
5. A failed candidate blocks its owning rule and triggers another planning pass.

The validation step checks only whether a candidate compiles. It does not ask Tailwind whether a better spelling exists.

## Proposed Flow

### 1. Probe candidates

The first native planning pass returns `candidateProbes` in addition to the existing applied `candidates` list. A probe records a candidate that reached a concrete rewrite site before quote-fit and source-edit checks.

This distinction matters for font families. An arbitrary font candidate can contain a quote that does not fit an existing HTML attribute. The migration still needs to discover that candidate because a generated `font-open-sans` alias removes the quote and may make the rewrite safe on the next pass.

`candidateProbes` is internal orchestration data. It does not change the public migration report.

### 2. Canonicalize with the target design system

TypeScript first builds a provisional design system that contains every generated media `@custom-variant` allocated for the group. A complete probe such as `twm-media-print:mr-[auto]` cannot be canonicalized against the original design system because that system does not know the generated variant yet. The provisional system may contain definitions that the final plan does not use; final entry pruning still keeps only applied definitions.

TypeScript calls `canonicalizeCandidates([candidate])` once per distinct probe against that provisional design system. Calling the method separately preserves a direct input-to-output mapping because the method may deduplicate a batch.

The orchestration layer accepts an alias only when Tailwind returns exactly one different candidate and the compiled candidate is runtime-stable. It keeps the original candidate when Tailwind returns zero results, more than one result, or the unchanged input. Existing candidate compilation validation remains authoritative after replanning.

A direct literal declaration is runtime-stable. A declaration backed by a theme custom property is runtime-stable only when the property resolves to the original value and the complete source snapshot contains no possible authored override outside Tailwind theme definitions. The reservation scan covers parsed stylesheet and inline-style declarations, planned arbitrary custom-property utilities, and source references that may write the property dynamically. An unparseable or dynamic write mentioning the property is an override reservation. An opaque dynamic write whose property name cannot be bounded reserves every theme-backed alias in that entry group.

For example, `p-[1rem]` may become `p-4` only when `--spacing` resolves to `.25rem` and no authored `--spacing` override exists. If `.compact { --spacing: 1rem }` is present, the migration keeps `p-[1rem]` because `p-4` would compute to `4rem` at that site.

Canonicalization applies to the complete candidate, so Tailwind preserves prefixes, stacked variants, arbitrary variants, and important modifiers:

```text
tw:hover:mr-[auto] -> tw:hover:mr-auto
md:max-w-[100%]!   -> md:max-w-full!
```

The planner receives accepted aliases through a new internal `candidateAliases` request field. Every rewrite path resolves the canonical spelling before it checks attribute quoting, records candidate matches, or creates an edit. The native planner transfers each original candidate's existing `candidate_properties` set to its alias and merges the sets when aliases deduplicate. Conflict detection uses that authoritative metadata instead of inferring properties from the canonical spelling. This prevents a generated family alias such as `font-open-sans` from being classified as font-weight only because it starts with `font-`. TypeScript does not reconstruct source properties from candidate strings, and the planner never performs textual replacement on an already-rendered source file.

### 3. Register custom font families

The native planner returns structured font-family probe metadata with the candidate, normalized family stack, parsed first-family kind, owning stylesheet, and rule identity. The first-family kind distinguishes a quoted or unquoted family name, an unquoted CSS generic keyword, and a CSS-wide keyword. TypeScript uses this structured value instead of parsing Tailwind candidate syntax.

TypeScript first canonicalizes the font candidate against the media-provisional design system. This can reuse an existing `--font-*` token, including one loaded through the entry's import graph, but canonicalization alone is not sufficient proof. TypeScript compares the compiled named and arbitrary candidates after resolving static theme variables. Their normalized declaration sets must match exactly. A named utility with companion declarations such as `font-feature-settings` or `font-variation-settings` is not equivalent to a source rule that declared only `font-family`.

When the candidate remains arbitrary, the entry is writable, and the first family is a parsed family name rather than a generic or CSS-wide keyword, TypeScript allocates a generated font token and appends a provisional `@theme` block:

```css
@theme {
  --font-open-sans: "Open Sans", sans-serif;
}
```

TypeScript loads the entry augmented with both generated media definitions and proposed font tokens through the existing `loadWith()` path, then canonicalizes the font candidate again. Canonicalization caches belong to one design-system instance, so this lookup cannot reuse a result from the system that did not contain the font token. Tailwind must return the expected named candidate, compile it, and produce the same normalized declaration set as the arbitrary candidate before the alias is accepted:

```text
[font-family:"Open_Sans",_sans-serif] -> font-open-sans
```

Unquoted CSS generic family keywords do not create tokens. The initial list follows the CSS Fonts specification and includes `serif`, `sans-serif`, `monospace`, `cursive`, `fantasy`, `system-ui`, `ui-serif`, `ui-sans-serif`, `ui-monospace`, `ui-rounded`, `math`, `fangsong`, and `emoji`. A quoted spelling such as `"serif"` is a family name rather than the `serif` generic keyword and remains eligible for registration. Named fonts such as Arial also remain explicit families because operating-system availability cannot be determined statically.

The CSS-wide keywords `initial`, `inherit`, `unset`, `revert`, and `revert-layer` never create font tokens. Tailwind may canonicalize their arbitrary property spelling, but the resulting utility must preserve the keyword directly instead of routing it through `var(--font-*)`.

A value based on `var()`, `env()`, interpolation, or another runtime-dependent construct keeps its arbitrary candidate or remains retained under existing safety rules.

### 4. Replan from the source snapshot

If canonicalization or font registration creates any alias, TypeScript runs the native planner again from the immutable source snapshot with:

- the candidate alias map;
- the original Tailwind entry source;
- the same blocked-rule state and media extraction state;
- the same source files and stylesheet inputs.

The second pass creates source edits with canonical class names. The first pass never edits the filesystem.

After replanning, TypeScript keeps only generated font tokens referenced by applied candidates and appends those definitions to the planner-composed entry. It does not need another native pass because removing an unused token cannot affect an applied candidate. Canonicalization and font registration therefore add at most one native replan.

Media-definition extraction, candidate compilation failures, and recoverable package failures may still trigger their existing bounded replans. Implementations must keep each reason explicit instead of sharing an unbounded retry loop.

## Font Token Naming

The TypeScript orchestration layer derives the token name from the first explicit family in the stack:

```text
"Open Sans", sans-serif -> open-sans
Acme Display, serif     -> acme-display
```

The allocator applies these rules:

1. Decode the CSS family name through the CSS parser.
2. Normalize Unicode to NFC and lowercase it.
3. Replace runs outside CSS identifier letters and numbers with one hyphen.
4. Trim leading and trailing hyphens.
5. Use `family` when no identifier characters remain.
6. Prefix a leading digit with `family-`.

The full variable is `--font-{name}`, and the emitted utility is `font-{name}`.

Token allocation is deterministic across a package-entry group:

1. Reuse the lexicographically first existing `font-*` candidate only when Tailwind canonicalizes to it, its complete normalized declaration set matches the arbitrary candidate, and its backing custom properties have no authored override reservation.
2. Reserve names whose complete prefixed candidate spelling already appears in the entry's effective Tailwind scan corpus, including discovered source files, entry-graph sources, and inline `@source` candidates. This prevents a new token from activating an inert semantic or custom-CSS class at unrelated sites.
3. Reserve names whose `--font-{name}` property appears in an ordinary declaration, a planned arbitrary custom-property utility, or a possible dynamic property write anywhere in the complete source snapshot.
4. Consider the base generated name only when no reservation owns it and its prefixed `font-{name}` candidate does not already compile in the pre-font provisional design system.
5. If a reservation, token, or existing Tailwind utility owns the base spelling, try `-2`, `-3`, and later numeric suffixes with the same collision checks.
6. Try at most 100 spellings, including the base name. Exhaustion emits `font-theme-registration-failed` and retains the owning rules.
7. After provisional registration, require Tailwind to canonicalize the arbitrary font candidate to the allocated candidate exactly, require declaration-set equivalence, and verify that no reserved custom property backs the candidate. Semantic rejection terminates allocation immediately because another suffix cannot change the candidate's behavior.
8. Reuse the assigned name for repeated occurrences of the same normalized stack.

The utility collision check prevents a generated token from changing an existing candidate's meaning. For example, the default design system already owns `font-bold` as a font-weight utility, so a family named `Bold` starts at `font-bold-2` instead of registering `--font-bold` and replacing the meaning of existing `font-bold` sites. The source reservations likewise prevent `font-open-sans` from being activated when that class or `--font-open-sans` already has an unrelated project meaning.

The comparison preserves the authored family order and fallback list. `"Open Sans", sans-serif` and `"Open Sans", Arial, sans-serif` require different tokens. Declaration-set comparison ignores selectors and formatting, resolves generated static `--font-*` references, and requires the same properties, normalized values, and importance. It rejects every additional effective declaration.

## Unwritable Entries

A generated font utility is valid only while the Tailwind entry contains its token. When the entry group is not writable, the planner retains every rule that requires a new font token and emits a stable warning:

```text
code: font-theme-registration-required
message: The font family requires a Tailwind theme token, but the Tailwind entry cannot be edited, so the rule is retained.
```

An unwritable entry may still use an existing font token because no entry edit is required.

`--force` does not bypass this rule. Retention is a supported conservative outcome rather than a package failure.

## Tailwind Entry Edits

Generated font tokens follow the existing append-only entry policy. The TypeScript orchestration layer must not rewrite existing imports or `@theme` blocks. It appends one deterministic block after existing content and preserves the entry's final newline convention.

When the same planning pass also moves keyframes or global at-rules and appends generated media definitions, final entry additions use this order:

1. planner-owned moved definitions;
2. generated font `@theme` block;
3. generated media custom variants.

TypeScript reloads the complete augmented entry in memory before accepting any edit. A load failure discards generated font aliases and retains their owning rules. Integrity failures keep their existing fatal behavior. The provisional design system used to choose font aliases includes every proposed font token, while the final design system includes only tokens referenced by applied candidates.

## Safety Invariants

1. Canonicalization must not change the CSS value represented by a candidate.
2. Tailwind's own `canonicalizeCandidates()` selects the spelling. `tw-migrate` does not maintain a parallel built-in utility table for this step.
3. Complete candidates that use generated media variants must be canonicalized against a design system containing those provisional definitions.
4. Every final candidate must pass `candidatesToCss()` against the final augmented design system.
5. Existing and generated font aliases must have the same complete normalized declaration set as their arbitrary candidate. Companion declarations are differences, not harmless additions.
6. A theme-backed alias must retain its arbitrary form when an authored or possible dynamic override can change any custom property it dereferences.
7. A generated font token must compile to the exact normalized family stack before its alias is used, and it must not replace the meaning of a candidate, class spelling, or custom property already present in the source snapshot or pre-font design system.
8. Canonical spelling and source-property metadata must remain paired inside the native planner so conflict detection does not infer an alias's property from an ambiguous class prefix.
9. Candidate aliases must be applied during planning, before source edits are rendered.
10. Replanning must start from the immutable snapshot.
11. An unused generated token must not remain in the Tailwind entry.
12. Font token allocation must terminate after a bounded number of ownership collisions or the first semantic rejection.
13. CSS-wide font-family keywords must remain direct values rather than generated token values.
14. A second migration run must produce no diff.
15. Dry-run must perform the same planning and validation without writing.
16. A missing canonicalization capability may preserve the original valid arbitrary candidate, but it must not guess a named candidate.

## Internal Data Changes

The TypeScript planner request gains an optional field:

```ts
interface PlannerRequest {
  candidateAliases?: Record<string, string>;
}
```

The native response gains internal fields that TypeScript strips from the public report:

```ts
interface NativePlan {
  candidateProbes: string[];
  fontFamilyProbes: Array<{
    candidate: string;
    value: string;
    firstFamily: {
      name: string;
      kind: "name" | "generic" | "css-wide";
    };
    stylesheet: number;
    ruleId: RuleSpan;
    authoredSpan: RuleSpan;
  }>;
}
```

The loaded Tailwind design-system interface gains the capability already exposed by Tailwind v4:

```ts
interface DesignSystem {
  canonicalizeCandidates(candidates: string[]): string[];
}
```

The implementation feature-detects this unstable method. When the target Tailwind v4 build does not expose it, non-font candidates keep their valid planner spelling. Generated font candidates still require explicit final compilation before use.

These fields remain private implementation details. The public `migrate()` options and report types do not change.

## Diagnostics

Canonicalization itself does not produce a warning because both the original and canonical candidate represent the same Tailwind utility.

New font-specific warnings are limited to cases where registration prevents conversion:

- `font-theme-registration-required`: the entry is not writable;
- `font-theme-registration-failed`: the augmented entry does not load, token-name allocation exhausts its bounded attempts, canonicalization does not select the generated utility, declaration sets differ, a late reservation invalidates the name, or the generated utility does not compile.

Existing candidate compilation, source integrity, selector safety, and write warnings retain their current codes.

## Performance

Single-candidate canonicalization operates on the distinct probe set for one loaded entry group. TypeScript keeps a separate complete-candidate cache for each design-system instance. Loading an entry with provisional media definitions, provisional font tokens, or the final pruned additions creates a new cache scope. It does not enumerate `getClassList()` and does not compile the full utility catalog.

Font registration may load the design system once with a provisional block and once more for the final planner-composed entry containing only used tokens. Most groups without new fonts require no extra design-system load.

The implementation should add a benchmark fixture with repeated declarations to verify that canonicalization work scales with distinct candidates rather than rule count.

## Testing Strategy

### Tailwind bridge tests

- canonicalize `mr-[auto]` to `mr-auto`;
- canonicalize `max-w-[100%]` to `max-w-full`;
- canonicalize a spacing value such as `p-[1rem]` to the target theme's named utility when `--spacing` has no authored override;
- preserve `p-[1rem]` when an authored or possible dynamic `--spacing` override exists;
- preserve `p-[13px]` when no named utility exists;
- preserve prefixes, variants, arbitrary variants, and important modifiers;
- canonicalize a candidate that uses a generated media variant against its provisional definition;
- keep the original candidate when canonicalization returns no single changed result;
- isolate cached results between original, media-provisional, and font-provisional design systems;
- reject an existing font token whose compiled utility adds feature or variation settings;
- validate aliases against the final design system.

### Planner tests

- return probes before quote-fit rejection;
- transfer and merge native `candidate_properties` when applying string aliases;
- preserve source-property metadata for conflict checks without a TypeScript round trip;
- classify `font-open-sans` as font-family when checking it against an existing `font-bold` weight utility;
- retain rules only for real generated-candidate conflicts, not a shared `font-` prefix;
- keep rule attribution stable across replans;
- deduplicate aliased candidates deterministically;
- retain blocked rules exactly as before.

### Font tests

- reuse an existing exact font token;
- register `"Open Sans", sans-serif` as `--font-open-sans` and emit `font-open-sans`;
- preserve the complete fallback stack in the token value;
- assign `open-sans-2` when `--font-open-sans` owns another value;
- assign `font-bold-2` for a family named `Bold` because the original design system owns `font-bold` as a font-weight utility;
- stop suffix allocation and retain the rule when post-registration semantic validation rejects a free spelling;
- stop after 100 ownership collisions when a plugin reserves every attempted `font-*` spelling;
- reject reuse of a matching existing font token with `font-feature-settings` or `font-variation-settings` companions;
- deduplicate the same stack across stylesheets sharing one entry;
- allocate distinct names for distinct stacks with the same first family;
- skip token generation for unquoted generic-only, CSS-wide, and runtime-dependent values;
- preserve `initial`, `inherit`, `unset`, `revert`, and `revert-layer` as direct arbitrary font values;
- register a quoted family name such as `"serif"` even though its spelling matches a generic keyword;
- suffix a font token when its candidate spelling already appears inertly in the effective scan corpus;
- suffix a font token when its custom property name appears in an ordinary declaration, planned arbitrary property, or possible dynamic write;
- retain the rule when the entry is unwritable;
- prune a generated token when selector or source safety retains every owning rule;
- convert a quoted font family at a double-quoted HTML site after the named alias removes the quote;
- preserve `@font-face` and external font loading behavior.

### End-to-end snapshots

Update the quoted-value fixtures that currently assert `[font-family:...]`. Add packaged CLI coverage for canonical margin and max-width output, generated entry changes, dry-run parity, write behavior, and a zero-diff second run.

## Implementation Sequence

1. Add candidate probes and alias application to the native planner with focused Rust tests.
2. Extend the Tailwind bridge type and add per-candidate canonicalization with Node tests.
3. Add runtime custom-property reservations and the bounded canonicalization replan to package-entry group planning.
4. Add structured font probes, scan-corpus reservations, token allocation, and provisional entry augmentation.
5. Add unwritable-entry retention and font diagnostics.
6. Update public API tests and packaged CLI snapshots.
7. Run focused Rust and Node suites, then `vp check`, `vp run test`, and `vp run test:snapshots`.

## Success Criteria

1. `margin-right: auto` emits `mr-auto` when the target Tailwind installation canonicalizes it.
2. `max-width: 100%` emits `max-w-full`.
3. Exact theme values use the target project's named utility only when authored custom-property overrides cannot change its runtime value.
4. Values without a runtime-stable named utility keep an exact arbitrary utility.
5. An explicit custom font stack emits a generated `font-*` utility only when Tailwind canonicalizes the original arbitrary candidate to that utility and compiles it.
6. Existing font tokens are reused only when their complete compiled declarations are equivalent and their backing properties cannot be overridden by authored CSS.
7. Existing utility spellings, inert scanned classes, and ordinary custom properties remain unchanged while naming collisions resolve deterministically within a bounded search.
8. Canonical font-family aliases retain their source-property identity through every conflict check without TypeScript reconstructing that metadata.
9. Canonicalization sees generated media variants and does not retain arbitrary inner values solely because their definitions are provisional.
10. Quoted family names remain distinct from unquoted generic and CSS-wide keywords.
11. Unwritable Tailwind entries retain rules that require new font tokens.
12. Prefixes, variants, important modifiers, source quoting, and warning attribution remain correct.
13. Preview and write produce the same plan, and a second run is a no-op.
14. No public API shape changes.

## Rejected Alternatives

### Expand the Rust utility table

A larger manual table would fix known examples but continue duplicating Tailwind behavior. It would drift across Tailwind v4 minor releases and project themes.

### Enumerate and reverse-index every Tailwind class

The current Tailwind installation exposes more than 23,000 classes. Compiling the catalog creates about 14 MB of CSS per entry before parsing. This cost is unnecessary for single-candidate canonicalization.

### Replace every arbitrary value with a generated theme token

Generating tokens for colors, spacing, and other one-off values would inflate the project's theme and require low-quality synthetic names. This RFC generates only the explicitly requested font tokens.

### Approximate with the nearest built-in value

Approximation changes rendered output. The migration requires exact value preservation.

### Edit planned source strings after Rust returns

Textual replacement can alter unrelated class substrings, break quoting, and invalidate byte-span attribution. The native planner must render the canonical candidate during a fresh pass.

## Deferred Work

- exact compound utility recognition such as `truncate`;
- bounded minimum-cover selection for declaration sets;
- optional reuse of explicitly opted-in project custom utilities;
- canonicalization telemetry and large-repository benchmarks;
- theme-token generation for non-font design values.
