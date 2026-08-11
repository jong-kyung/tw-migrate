# RFC: Media Query Definition Extraction

## Status

Implemented

## Summary

`tw-migrate` currently converts media queries to Tailwind built-in variants when their conditions match known media features or existing theme breakpoints. Other representable media queries become arbitrary variants such as `[@media_screen_and_(width_<=_768px)]:m-0`.

Arbitrary variants preserve behavior, but the repeated query text makes migrated class values long, duplicated, and hard to edit. This RFC decomposes media conditions into their components and gives each component a stable named `@custom-variant` in the project's Tailwind CSS entry:

- an `and`-joined media condition splits into components, and consumers use idiomatic stacked variants such as `screen:width-lte-768px:m-0`;
- each component reuses a Tailwind built-in variant or an existing project breakpoint before a definition is generated;
- conditions that cannot decompose, such as comma lists and negated compounds, become one whole variant with a joined readable name;
- a component or condition whose readable name is unavailable falls back to a digest name in the reserved `twm-media` namespace;
- workspace packages without their own entry may share an ancestor entry only when a static source relationship proves that the entry covers the package;
- extraction runs by default and has an opt-out CLI and API option; and
- an entry that cannot be edited safely keeps the current arbitrary-variant behavior.

The migration writes only definitions used by the final validated plan. Tailwind entry edits join the existing snapshot verification and atomic write transaction.

This RFC supersedes the unmatched `@media` behavior in the core RFC's **Breakpoints and At-Rules** section. It does not change `@supports`, `@container`, or `@starting-style` handling.

## Goals

1. Replace repeated arbitrary media variants with short stacked named variants.
2. Define each distinct media condition component once per Tailwind entry.
3. Reuse Tailwind built-ins and existing theme breakpoints per component before generating definitions.
4. Decompose only where equivalence is provable: variant stacking nests `@media` blocks, and nesting is exactly `and`.
5. Keep generated names deterministic and derived from conditions rather than inferred device labels.
6. Include Tailwind entry changes in dry-run previews, integrity checks, and transactional writes.
7. Preserve the current arbitrary variant as the safe fallback when definition extraction is disabled or unavailable.
8. Keep a second migration run byte-identical.
9. Resolve and update one ancestor-owned Tailwind entry for every workspace package with a proven source relationship to it.

## Non-Goals

1. Inferring semantic names such as `mobile`, `tablet`, or `desktop` from width values.
2. Converting between `px`, `rem`, `em`, or other units.
3. Replacing existing project breakpoint names with generated names.
4. Replacing Tailwind built-in variants such as `dark`, `print`, `motion-reduce`, or `portrait`.
5. Generating `--breakpoint-*` theme variables for unmatched minimum-width values; a new lower bound becomes a component variant, and theme-breakpoint generation is deferred work.
6. Aliasing a condition to a differently named authored `@custom-variant` such as an existing `tablet`; a definition identical to what the migration would emit is adopted, and aliasing is deferred work.
7. Extracting `@supports`, `@container`, `@starting-style`, or selector arbitrary variants.
8. Expanding HTML `<link media>` support in the first implementation.
9. Moving generated definitions into imported theme files or reorganizing existing `@theme` blocks.
10. Editing dependency-owned or symlinked Tailwind entries.
11. Adding a user-configurable naming template in the first implementation.
12. Inferring that a sibling package's Tailwind entry consumes another package.
13. Reusing an ancestor Tailwind entry outside `--workspaces` mode.
14. Keeping digest names stable across tw-migrate or toolchain releases. The migration is a one-shot tool, and determinism guarantees apply to reruns of one release.

## Terminology

- **Existing variant**: a Tailwind built-in variant or a variant backed by the project's existing theme tokens.
- **Condition component**: one atomic part of an `and`-joined media condition: a media type with its optional `only`, one parenthesized feature condition, one bound of a range, or one atomic negation such as `not (hover)`.
- **Component key**: the deterministic normalized representation of one component, used for deduplication and naming.
- **Whole condition key**: the normalized representation of a complete condition that cannot decompose.
- **Generated component variant**: a new `@custom-variant` wrapping one component.
- **Generated whole variant**: a new `@custom-variant` wrapping one complete non-decomposable condition.
- **Media definition**: either generated form.
- **Digest name**: `twm-media-<digest>`, the reserved-namespace fallback name derived from a stable digest of the key.
- **Extraction fallback**: use of the current arbitrary at-rule variant instead of changing the Tailwind entry.
- **Verified ancestor-shared entry**: an entry owned by an ancestor package with static proof that a workspace package with no entry of its own loads the entry's output and falls inside the entry's utility scan scope.
- **Entry group**: selected workspace packages that resolve to the same Tailwind entry and therefore share one definition registry and ordered entry plan.

## User-Visible Behavior

### Decomposition

An `and`-joined, single-branch media condition splits into components. Given:

```css
@media screen and (width <= 768px) {
  .card {
    margin: 0;
  }
}
```

The Tailwind entry gains one definition per missing component:

```css
@custom-variant screen {
  @media screen {
    @slot;
  }
}

@custom-variant width-lte-768px {
  @media (width <= 768px) {
    @slot;
  }
}
```

A migrated consumer uses stacked variants: `screen:width-lte-768px:m-0`. Stacked media variants compile to nested `@media` blocks, and nesting is exactly conjunction, so the meaning is preserved. Components stack in their authored order.

A double range decomposes into its two bounds: `(48rem <= width < 60rem)` becomes the components `width >= 48rem` and `width < 60rem`, so each bound's definition is shared with every other condition that uses it.

### Per-component resolution order

For each component, the planner tries these forms in order:

1. a verified Tailwind built-in media variant, so `screen and (prefers-color-scheme: dark)` becomes `screen:dark:` when `dark` still means that media condition;
2. an exact existing project breakpoint: an inclusive lower bound whose value matches a breakpoint uses `<name>:`, and an exclusive upper bound whose value matches uses `max-<name>:`, so `(48rem <= width < 64rem)` with `md: 48rem` and `lg: 64rem` becomes `md:max-lg:`;
3. a generated component variant with a readable name; and
4. a generated component variant with a digest name when the readable name is unavailable.

Built-in names are not fixed keywords: a project may redefine one, most commonly `@custom-variant dark (&:where(.dark, .dark *))` for class-toggled dark mode, and the redefined name then expresses a selector rather than the authored media condition. Reusing a built-in therefore requires proof: a compiled probe must show that the loaded design system's effective expansion of the name equals the expected media condition. A redefined name is never reused; its condition receives a generated component variant such as `prefers-color-scheme-dark` instead. This verification applies everywhere the resolution order runs, including single-condition queries that the planner already converts today, because mapping a condition onto a redefined name changes rendered behavior on either path; the resulting output change for redefined built-ins is a deliberate behavior-preserving fix. Existing breakpoint reuse requires the same proof, because `@custom-variant` can shadow a breakpoint name with unrelated semantics even while the theme token keeps its value; a shadowed breakpoint name is never reused and the component receives a generated variant. Every breakpoint-matching query therefore flows through decomposition and verification rather than being converted whole, and the shipped planner's legacy approximation, which maps an inclusive `(max-width: X)` onto the exclusive `max-*` form of the next breakpoint by adding a sub-pixel epsilon, does not carry into extraction: the inclusive bound is preserved exactly through a generated variant, because the approximation changes behavior in the sub-pixel window between the bound and the breakpoint.

Breakpoint values compare as case-insensitively identical text with no numeric parsing, so `48REM` proves `48rem` while `48.0rem` does not; an unproven spelling simply receives a generated variant with identical meaning.

The first two steps keep current output stable for unredefined names and extend reuse into compound conditions, which today fall to arbitrary variants whenever any part is unmatched.

### Non-decomposable conditions

Variant stacking expresses only conjunction, so three shapes cannot decompose:

- a comma list such as `screen, print`, which means either branch;
- an `or`-joined condition such as `(color) or (hover)`; and
- a negation spanning more than one component, such as `not screen and (color)`, because the negation of a conjunction is a disjunction.

Such a condition becomes one generated whole variant preserving the complete condition verbatim. Its readable name joins the component tokens: `screen-or-print` and `not-screen-and-color`. An atomic negation such as `not (hover)` is a single component named `not-hover` and needs no whole variant.

### Naming and the digest fallback

A readable name is used only when it derives cleanly: it consists of lowercase letters, digits, and hyphens, where a value contributes only letters, digits, and `p` for a decimal point, and the complete name fits the fixed length limit. A component or condition whose name would need any other character, such as a `calc()` or `env()` value, or whose name is too long, is not mangled or truncated; it takes the digest name directly.

The digest name is `twm-media-<digest>`, where the digest is a content digest of the normalized key rendered as sixteen lowercase hex digits, deterministic across runs of one tw-migrate release. In a compound condition only the affected component falls back, so `screen and (min-width: calc(100vw - 2rem))` still migrates as `screen:twm-media-<digest>:`.

A readable name that is already owned with a different meaning falls back to the digest name the same way, and a digest name owned with a different meaning falls back to the arbitrary variant for that condition. A name whose existing definition is identical to what the migration would emit is adopted instead, whoever wrote it, keeping a second run diff-free.

### Single-use conditions

Extraction does not require repetition. Every unmatched media component receives a named definition when the entry is writable and the final plan uses that definition. Identical keys share one definition.

This policy favors readable migrated class values. It accepts that a Tailwind entry may contain a definition used by one migrated class.

### Workspace shared entries

Before package planning, workspace discovery builds a catalog of Tailwind entries owned by each package. Entry resolution follows these rules:

1. use the package's own unique entry when it has one;
2. in `--workspaces` mode, when the package has no entry, inspect entries owned by ancestor packages from nearest to farthest;
3. accept an ancestor entry only when a supported static relationship proves that the child loads its output and that its utility scan covers the child package;
4. select the nearest ancestor level with exactly one proven entry;
5. fail the package as ambiguous when that level has multiple proven entries;
6. fail the package when no ancestor entry has a proven relationship; and
7. never select an entry owned by a sibling or descendant package.

Package ancestry identifies candidates but does not prove consumption. Selecting an ancestor entry requires both of these proofs:

- **Loading proof**: a child-owned writable JavaScript, TypeScript, Vue, or HTML source directly imports or links the ancestor Tailwind entry, and every consumer of the migrated stylesheet is statically reachable from such a source through child-owned imports, so the entry's CSS output provably reaches the flows being rewritten.
- **Scan proof**: the entry's utility detection provably covers the child package, so utilities generated for migrated child sources are emitted. A literal `@source` or `source(...)` scope that contains the child package proves this. Automatic detection also proves it when its base directory contains the child package and the entry does not disable detection with `source(none)`, but containment alone is not sufficient: automatic detection honors ignore rules such as `.gitignore` patterns even for Git-tracked files, so each rewritten consumer path must also pass the effective scanner rules. A consumer excluded by those rules fails scan proof and its stylesheet keeps its rules retained. A literal scope does not need this check because explicit `@source` paths are scanned regardless of ignore rules.

Neither proof substitutes for the other. A direct import does not prove that the child's newly generated utility classes are scanned, and a `@source` scope does not prove that the child loads the entry's output, because a scanned library may be consumed by a different application that never loads this entry. An entry whose literal source scope excludes the child fails scan proof even though the child imports it. Dynamic paths, package dependency declarations, and a shared repository root prove neither fact.

Loading proof is scoped to consumer flows rather than granted package-wide. A Storybook, demo, or test source that imports the entry proves loading only for the consumers it reaches through static imports. In-repository reachability also cannot speak for consumers that leave the repository: a consumer exposed through the child's package entry points, such as the `exports`, `main`, or `module` fields, can be imported by an external application that never loads this entry, so an exported consumer counts as an unproven flow regardless of which in-repository sources reach it. A stylesheet with a consumer outside every proven flow keeps its rules retained even when the rest of the package migrates.

Explicit package-to-entry mappings remain deferred. A package where no stylesheet holds both proofs fails entry resolution and stays unchanged, or is skipped as a recoverable package failure under `--force`.

Default package mode retains the current ownership boundary and does not inspect ancestor entries.

The entry owner supplies the project root used to resolve Tailwind, imported stylesheets, plugins, and theme configuration. The migrated child package continues to supply its own stylesheets, consumers, preprocessors, and failure attribution.

Packages that resolve to the same entry form one entry group. The group allocates media names before consumer planning, processes package plans in normalized package-path order, and produces one Tailwind entry edit.

## Decomposition Rules and Representability

A condition decomposes when it is one branch whose parts are joined only by `and`. Its components are:

1. the media type with its optional `only` prefix, such as `screen` or `only screen`;
2. each parenthesized feature condition, in `(feature: value)`, `(feature)`, or range form;
3. each bound of a double range, read from the feature's side; and
4. an atomic negation of exactly one such part.

Decomposition never reorders components, never merges bounds, and never rewrites a negation, because `not` distributes over `and` only by becoming `or`, which stacking cannot express.

A condition is representable when the parser proves its complete grammar: unknown syntax, nested condition grouping, custom-media references, comments in the prelude, and characters that a generated definition cannot carry stay on the existing arbitrary-variant and retention paths. Comments are rejected rather than rewritten because their placement can be significant inside function values such as `calc()`. A media type may be any valid ASCII CSS identifier except the reserved keywords `not`, `and`, `only`, `or`, and `layer`, so an unknown type such as `tv` keeps its authored match-nothing behavior verbatim; exotic identifiers such as non-ASCII names stay on the arbitrary-variant path.

Nested `@media` rules keep their nesting order through stacked variants. The planner assigns each distinct component its own definition rather than combining nested conditions into a new synthetic query.

## Normalization and Deduplication

The planner derives a key from each parsed component, and from the complete condition when it cannot decompose. Normalization may:

- collapse ASCII whitespace, which is exactly the whitespace CSS defines;
- case-fold CSS keywords, media feature names, and units, which CSS defines as case-insensitive;
- normalize legacy `min-width` and `max-width` forms to their exact inclusive range equivalents;
- flip a descending range or a value-first comparison to the provably equivalent feature-first ascending form; and
- fold nothing else: values keep their authored spelling.

Normalization must not:

- convert units;
- assume a root font size;
- reorder components or comma-separated branches;
- simplify calculations;
- fold case in values that carry case-sensitive tokens, such as `env()` arguments;
- canonicalize numeric spellings: `+52rem`, `5.2e1rem`, and `52rem` keep distinct keys, and the worst outcome is one duplicate definition per authored spelling; or
- merge conditions whose equivalence the parser cannot prove.

For example, `48rem` and `768px` remain different keys even when a typical browser configuration would render them at the same width.

Deduplication happens per resolved Tailwind entry. Before consumer planning, an entry group collects every key from all member packages and allocates one definition set. Packages with separate entries receive separate definitions.

## Generated Names

### Component names

A component name uses the media type, feature name, comparison operator, and value. Operators use stable identifiers `lt`, `lte`, `gt`, and `gte`, and `p` encodes a decimal point in values.

| Component              | Variant name        |
| ---------------------- | ------------------- |
| `screen`               | `screen`            |
| `only screen`          | `only-screen`       |
| `(width <= 768px)`     | `width-lte-768px`   |
| `(min-width: 47.5rem)` | `width-gte-47p5rem` |
| `(hover: hover)`       | `hover-hover`       |
| `(color)`              | `color`             |
| `not (hover)`          | `not-hover`         |

### Whole names

A non-decomposable condition joins its component tokens:

| Condition                | Variant name           |
| ------------------------ | ---------------------- |
| `screen, print`          | `screen-or-print`      |
| `(color) or (hover)`     | `color-or-hover`       |
| `not screen and (color)` | `not-screen-and-color` |

### The digest fallback

Names must be valid Tailwind variant identifiers. When a readable name cannot derive cleanly, exceeds the fixed length limit, or is already owned, the definition takes the digest name `twm-media-<digest>` with no intermediate form: there is no character mangling, no prefix truncation, and no suffix ladder.

Name reservation follows one principle: every name the migration could emit or activate is checked against the complete loaded design system, and an existing owner blocks the name regardless of where that owner lives. Three sources feed the reservation set:

1. **Parsed definitions**: the native preflight parses every project-owned stylesheet retained from the entry import graph and returns all authored `@custom-variant` names, while existing theme breakpoint names remain reserved through the theme-token graph.
2. **The loaded design system**: before committing to any name, the orchestration layer compiles a probe candidate against the unaugmented design system, which also reveals variants registered by an `@plugin` or defined in dependency-owned imported stylesheets.
3. **The scanned candidate corpus**: the probe cannot see inert candidates such as `width-lte-768px:hidden` sitting in a file inside the entry's detection scope, which a generated definition would activate. The preflight therefore scans the entry's provable detection scope for candidate tokens, parses each candidate's variant chain recursively, and reserves every referenced variant name rather than only literal prefixes. When the detection scope cannot be enumerated, generated names for the entry are not allocated and its conditions fall back to arbitrary variants.

The migration never overwrites or renames an existing definition, and adoption is decided by content rather than authorship. When the entry graph already contains a definition whose name and normalized meaning are exactly what the migration would emit, and the loaded design system's effective expansion of that name proves the same meaning, that definition is adopted as-is and nothing new is emitted, because using it is semantically indistinguishable from using the migration's own output. The expansion proof guards against a plugin or dependency registering the same name with different effective semantics behind an identical-looking stylesheet definition; such ambiguous ownership falls back instead of adopting. This one rule also makes a second run recognize the first run's definitions without any provenance marker. The comparison uses the normalized condition key rather than raw bytes, so reformatting an adopted block does not break recognition. A name owned with any different meaning is never touched: a readable name falls back to the digest name, and a digest name owned with a different meaning sends the condition to the arbitrary variant, so a name is claimed by at most one key and every later claimant falls back. Aliasing a condition to a differently named authored definition, such as an existing `tablet`, remains deferred work.

Name allocation runs once for the complete entry group before any consumer candidate is produced. Different keys from different packages therefore cannot independently claim the same name. The fixed key-to-name map is passed into every native package plan.

## Tailwind Entry Editing

The resolved Tailwind CSS entry becomes a planned source file whenever the final composed source differs from its snapshot. Media definitions, moved keyframes, and moved global at-rules all count as such changes, so an entry that gains only moved blocks is still emitted even when no media condition needed a generated definition or every generated definition was replanned to an arbitrary variant. A group whose composed source equals the snapshot emits no entry file.

The editor follows these rules:

1. keep every existing byte unchanged;
2. append missing `@custom-variant` definitions at the top level after existing content, each as its own block;
3. emit definitions in the proven cascade order defined below;
4. add no definition that lacks a final validated candidate; and
5. recognize existing matching definitions so a second run produces no diff.

Definition order is behavior rather than formatting. Tailwind emits utilities for stacked custom variants in variant registration order, so entry order decides which of two conflicting declarations wins, exactly as authored rule order did before migration. Rule order is proven only inside one compiled stylesheet. Across stylesheets and packages, the import or bundler order that loaded the original files decides the cascade, and filesystem or normalized package-path order does not prove it. The entry group therefore orders generated definitions by the position of the first migrated rule that uses each key inside its stylesheet, and applies normalized package-path order across stylesheets only as a deterministic tiebreaker that carries no cascade meaning.

Conflict analysis treats two candidates as ordering-sensitive when they set conflicting declarations on the same proven element under distinct media conditions that are not provably mutually exclusive. Such a pair migrates only when both rules come from one stylesheet and the emitted definition order provably reproduces the original winning rule through Tailwind's variant ordering; otherwise the pair is retained with the existing conflict warning. An ordering-sensitive pair that spans stylesheets has no proven load order and is retained. A pair in which one side uses only built-in or project-defined variants has no proven order against generated definitions and is retained the same way.

An entry group owns one mutable in-memory entry source initialized from the shared snapshot. Each package plan receives the current source and may return a new source containing moved keyframes or global at-rules. The orchestration layer adopts that planned source before adding media definitions or processing the next package. It removes intermediate Tailwind entry files from package plans and emits only the group's final composed entry file. Media extraction must never add a second planned file for a path already changed by the native planner.

Moved keyframes and global at-rules follow the same proof standard as generated definitions. Order is proven only inside one compiled stylesheet, where moved blocks keep their source order. When two distinct stylesheets move colliding order-sensitive definitions, such as registrations for the same `@property` name or overlapping `@page` rules, their original precedence depends on the import order that loaded those stylesheets. Neither stylesheet-path nor normalized package-path order proves that, and belonging to one package proves nothing more, because a package's modules are still loaded by consumer imports. The composition detects such collisions across every pair of distinct stylesheets and retains the affected modules and at-rules instead of emitting an order that may flip the winning definition.

The same gate covers definitions that stay in place. When a moved definition would collide with an order-sensitive definition already present in the entry or its imported stylesheets, the original precedence between the migrating module and the entry is equally unproven, while appending would always make the moved rule win. Such a definition is retained in its module instead of moved.

Imported theme files remain untouched. Generated definitions live in the resolved entry because that file already defines the design system used to validate migration candidates.

## CLI and Public API

Extraction is enabled by default.

The CLI adds:

```text
--no-extract-media-queries  Keep unmatched media queries as arbitrary variants.
```

The public API adds:

```ts
interface MigrateOptions {
  extractMediaQueries?: boolean;
}
```

`undefined` and `true` enable extraction. `false` preserves current arbitrary-variant behavior and never emits an extraction fallback warning.

The option composes with positional stylesheet migration, package migration, `--workspaces`, `--force`, and `--dry-run`. In workspace mode, ancestor entry reuse requires the static proof above and does not add a package-to-entry mapping option. A dry run includes each changed owned or verified ancestor-shared Tailwind entry in `changedFiles` and the unified diff but does not write it.

## Planning Architecture

### Native planner

Planning has separate collection and rewrite passes.

The collection pass parses one package without producing edits. It returns:

- normalized component and whole-condition keys with their stable digests;
- the decomposition, preferred readable name, and built-in or existing-breakpoint match for each key; and
- authored `@custom-variant` names from the Tailwind import-graph sources supplied by the TypeScript layer.

The TypeScript entry-group allocator combines every package collection result with the reservation set and produces one fixed key-to-name map. The rewrite pass receives that map and associates generated consumer candidates, as stacked variant chains, with the definitions they require. A package plan cannot allocate or rename a media definition independently.

These fields are internal orchestration data and do not change `MigrationReport`.

### TypeScript orchestration

The TypeScript layer:

1. discovers Tailwind entries for every package before calling `planPackage()`;
2. proves owned and ancestor-shared entry relationships and records each entry's owning package;
3. groups selected packages by resolved entry;
4. determines whether the entry group permits any entry edit;
5. resolves Tailwind and retains the complete project-owned CSS import graph from the entry owner's package root;
6. runs native condition collection for every package in the group;
7. reserves authored, theme, plugin, dependency, and scanned-corpus names and fixes the group's key-to-name map;
8. prevalidates each missing generated definition and marks rejected conditions for arbitrary fallback;
9. processes each group in normalized package-path order while carrying one mutable entry source and the fixed name map;
10. folds each permitted native planner entry result into the group's current source;
11. builds and validates the final augmented entry source;
12. removes definitions unused after compile-failure replanning; and
13. adds one final entry edit per writable group to the merged transaction.

Intermediate package plans must not claim the shared entry. The final group edit is the only Tailwind entry file passed to `mergePlans()`, which continues to reject accidental duplicate path claims.

Package-local Sass, Less, and Vue compiler loading remains rooted at the migrated package. Tailwind loading uses the entry owner because that package owns the entry's imports, plugins, dependency resolution, and theme. No bundled Tailwind compiler or new dependency is introduced.

### Candidate validation and replanning

Generated named variants must compile before any file is written. The orchestration layer first loads each missing definition against the current entry independently, and validates each stacked combination the plan uses. A definition rejected by the target Tailwind v4 version is removed from the group name map, and its condition uses the equivalent arbitrary variant. If the arbitrary candidate also fails, existing `candidate-compilation-failure` retention applies.

After package planning composes permitted keyframes, global at-rules, and surviving media definitions, the final augmented entry must parse and load successfully. If it fails, orchestration isolates generated media definitions against the composed base, falls back each rejected condition to its arbitrary form, replans affected consumers, and validates the final source again. Fallback replanning may change more than consumer candidates: when a replanned package retains a module whose keyframes or global at-rules were already folded into the group source, the previously adopted blocks are stale. Whenever replanning changes any package plan, orchestration rebuilds the complete entry-group composition from the shared snapshot using the updated package plans instead of editing the previously composed source, so no moved block survives for a rule that is no longer migrated. A final entry that remains invalid after all generated media definitions have been removed is fatal because the failure comes from the composed base or another planner defect rather than an extractable media condition.

## Safety and Fallbacks

The planner uses arbitrary variants when extraction is enabled but the resolved Tailwind entry is known before planning to be unsafe to edit, including when it is:

- a symbolic link;
- outside the writable project scope; or
- shared with an unselected package in a way that prevents one complete transaction.

Entry safety gates every entry mutation, not only media extraction. An unsafe entry group passes `entryWritable: false` to native collection and rewrite requests. The native planner must then:

- use arbitrary media variants instead of generated definitions;
- disable keyframe and global at-rule movement;
- retain CSS rules and module files whose definitions cannot remain in place safely; and
- return no planned file for the Tailwind entry.

The orchestration layer rejects any unsafe-group plan that still claims the entry path. It never adopts or emits an intermediate entry source for that group.

The migration emits one `media-query-definition-fallback` warning per affected Tailwind entry. The warning uses `(0, 0)` offsets and reports that media behavior was preserved with arbitrary variants because the entry was not edited. Existing animation and at-rule warnings explain any rules retained because their definitions could not move.

The fallback does not weaken existing integrity rules:

- a source change after snapshotting remains fatal;
- a write or rollback failure remains fatal;
- `--force` does not hide integrity or transaction failures; and
- file permissions remain preserved.

If extraction is disabled explicitly, arbitrary variants are expected behavior and produce no fallback warning.

A recoverable failure in one child package may be skipped under `--force` without discarding successful siblings in the same entry group. Entry discovery, Tailwind loading, or augmented-entry validation failures affect every package that depends on that shared entry and therefore skip the complete entry group only when the existing `--force` policy classifies the failure as recoverable. Integrity and write failures remain fatal for the full migration.

## Determinism and Idempotency

Keys, generated names, definitions, candidates, warnings, and entry edits use stable sorting. Naming does not depend on discovery order or hash-map iteration.

These guarantees apply to reruns of one tw-migrate release. A digest name may differ after a tool or toolchain upgrade; the worst outcome is one duplicate definition for a condition an earlier release already defined, which content-identity adoption and the resolver's name claiming contain.

A successful migration followed by the same command must produce:

- no new media definitions;
- no consumer changes;
- an empty diff; and
- the same warning set, excluding warnings tied to work completed by the first run.

## Diagnostics

This RFC adds one warning code:

| Code                              | Meaning                                                                         |
| --------------------------------- | ------------------------------------------------------------------------------- |
| `media-query-definition-fallback` | The Tailwind entry could not be edited safely, so arbitrary variants were used. |

Existing warning codes continue to cover unsupported media syntax, candidate compilation failures, retained rules, source-map ambiguity, and write integrity.

## Delivery

### Phase 1: Decomposition, naming, and internal planning

- Add component and whole-condition keys with exact normalization and decomposition rules.
- Add the per-component built-in, existing-breakpoint, readable-name, and digest resolution order.
- Add the native collection pass and group-supplied key-to-name map.
- Reserve authored, theme, plugin, dependency, and scanned-corpus names.
- Order generated definitions by proven cascade position and extend conflict analysis to overlapping conditions.
- Keep extraction disabled internally until entry augmentation and packaged coverage are available.

### Phase 2: Entry augmentation, options, and packaged behavior

- Add the CLI opt-out and public API option.
- Discover owned entries before package planning and require proof for ancestor entries in workspace mode.
- Group packages by entry and resolve Tailwind from the entry owner.
- Carry one entry source through ordered package plans, preserving permitted keyframes and global at-rules.
- Block every entry edit and retain dependent rules for unsafe groups.
- Prevalidate generated definitions and stacked combinations, and fall back rejected conditions.
- Load and validate the final augmented design system in memory.
- Add one final Tailwind entry file per writable group to snapshot verification and the atomic write transaction.
- Land packaged CLI fixtures and snapshots for help, preview, write, opt-out, fallback, workspace, and second-run behavior in the same change.
- Update public documentation when this phase lands.

### Phase 3: Browser ecosystem coverage

- Add controlled browser cases at, below, and above component boundaries, including stacked combinations.
- Cover separate and verified shared workspace entries.
- Exercise overlapping generated variants and entry-owner builds.

Phase 3 adds runtime confidence without changing the CLI contract introduced and snapshot-tested in Phase 2.

## Testing Strategy

### Rust planner tests

- existing built-in media variants remain unchanged when their default meaning holds;
- a redefined built-in is never reused and its condition receives a generated component variant;
- exact existing breakpoints remain unchanged;
- built-in and existing-breakpoint reuse applies per component inside compound conditions;
- decomposition preserves component order and produces stacked candidates whose nesting reproduces the authored condition;
- double ranges decompose into shared bound components;
- comma lists, `or`-joined conditions, and compound negations produce whole variants and are never decomposed;
- readable names derive only from clean tokens, and function values, overlong names, and collisions take the digest name directly;
- inclusive and exclusive operators receive distinct keys and preserve their conditions;
- normalization folds case and ASCII whitespace only: distinct numeric spellings keep distinct keys and their authored forms;
- duplicate keys share one definition across packages in one entry group;
- ordering-sensitive overlapping conditions migrate only with a proven winner and are otherwise retained; and
- extraction-disabled planning reproduces current arbitrary candidates.

### Node and API tests

- the resolved Tailwind entry receives only definitions used by final candidates;
- augmented candidates, including stacked combinations, compile with the target project's Tailwind v4 installation;
- `extractMediaQueries: false` leaves the entry unchanged;
- dry-run reports the entry diff without writing;
- write mode changes the entry and consumers in one transaction;
- source changes and write failures preserve current fatal behavior;
- symlinked or out-of-scope entries use arbitrary variants, never receive an entry edit, and warn;
- unsafe entries retain rules that require movable keyframes or global at-rules;
- generated definitions rejected by the target Tailwind version fall back independently;
- a digest name owned with a different meaning sends its condition to the arbitrary variant, and identical-content definitions are adopted regardless of authorship;
- built-in reuse is verified against the loaded design system's effective expansion;
- variant names contributed by plugins or dependency stylesheets are reserved through design-system probing;
- variants and recursively referenced names found in the entry's scanned candidate corpus are reserved before allocation;
- fallback replanning that retains a module rebuilds the composed entry without that module's moved blocks;
- a package-owned entry wins over an ancestor entry;
- a package without an entry uses an ancestor entry only when a supported static relationship proves coverage in workspace mode;
- a direct entry import without proven scan coverage fails child entry resolution;
- a consumer excluded by effective scanner rules such as `.gitignore` patterns fails automatic-detection scan proof;
- a literal `@source` scope without a child-owned import of the entry fails child entry resolution;
- an entry import from a demo or test flow does not authorize stylesheets whose consumers sit outside that flow;
- an exported consumer counts as an unproven external flow and keeps its stylesheet retained;
- missing or ambiguous ancestor proof fails the child package deterministically;
- sibling entries and implicit source coverage are never inferred;
- Tailwind loads from the entry owner while preprocessors load from each migrated package;
- colliding order-sensitive global at-rules moved from two distinct stylesheets are retained regardless of package;
- a moved at-rule that collides with an order-sensitive definition remaining in the entry graph is retained in its module;
- shared workspace entries receive one merged edit that preserves native keyframe and global at-rule additions; and
- a second run produces no diff.

### Packaged snapshots

Snapshots cover:

- CLI help for `--no-extract-media-queries`;
- stacked component output and whole-variant output;
- unchanged built-in and existing breakpoint output, including reuse inside compounds;
- deterministic definition ordering and digest fallbacks;
- authored-name reservations;
- generated-definition and unsafe-entry fallback diagnostics;
- retained keyframes and global at-rules for unsafe entries; and
- exact workspace deltas for preview and write modes.

### Browser ecosystem tests

A controlled fixture captures computed styles:

- below, at, and above an inclusive max-width boundary;
- across a stacked compound condition's component boundaries; and
- under a non-decomposable condition represented by a whole variant.

The pre- and post-migration captures must match.

## Success Criteria

1. Output for built-ins that keep their default meaning and for exactly matched project-defined breakpoints does not change. Two deliberate behavior-preserving exceptions apply: a name redefined with different semantics stops being reused, and the legacy epsilon mapping of inclusive upper bounds onto `max-*` retires in favor of exact preservation.
2. Built-ins and existing breakpoints are reused per component inside compound conditions, always with a verified effective expansion.
3. Every safely representable unmatched media condition migrates to stacked named variants, or to one whole named variant when it cannot decompose, when the entry is writable.
4. Decomposition and generated definitions preserve media types, operators, values, and compound structure exactly.
5. No unit conversion or device-name inference occurs.
6. Every named candidate and stacked combination compiles against the augmented target-project design system.
7. Dry-run and write mode use the same final plan.
8. Unsafe entry scope falls back to behavior-preserving arbitrary variants.
9. Tailwind entry and consumer edits commit or roll back together.
10. Repeated keys share one definition, shared workspace entries receive one edit, and a second run is a no-op.
11. A workspace package without an entry uses an ancestor entry only when a supported static relationship proves that the entry covers the package.
12. Entry groups allocate names before consumer planning and preserve all permitted keyframes, global at-rules, and media definitions in one composed source that is rebuilt from the snapshot whenever replanning changes a package plan.
13. Existing names from any design-system source, including plugins, dependencies, and inert scanned candidates, are never overwritten or activated with a changed meaning.
14. Unsafe entry groups produce no entry edit and retain every rule that depends on a definition that cannot move.
15. Generated definitions rejected by the target Tailwind version fall back per condition before a final entry failure becomes fatal.
16. Ordering-sensitive conflicts involving generated definitions or moved global definitions preserve the original winner or retain the affected rules.

## Accepted Trade-offs

1. A single-use unmatched component adds a Tailwind definition. Readable consumer classes take priority over minimizing entry length.
2. Generated names describe conditions rather than product semantics. `width-lte-768px` is longer than `tablet`, but it does not invent a design-system meaning.
3. A digest name such as `twm-media-4be27e9a51c03d88` is opaque; the definition in the entry, not the name, documents the condition. This affects only conditions whose values or shapes cannot produce a clean name.
4. A differently named authored definition with the same meaning is not aliased, so the entry may gain a generated definition duplicating an authored one under another name. Adoption applies only to identical-content definitions, and aliasing is deferred rather than proven now.
5. A new minimum-width bound becomes `width-gte-52rem:` rather than the theme-breakpoint idiom `min-52rem:`, because generating `--breakpoint-*` variables would reintroduce unit-namespace proofs, value-order gates, and custom-property reservations for a small notational gain.
6. Conditions that differ only through unit conversion remain separate even when they render similarly in one environment, and so do numeric spellings the parser does not prove identical, such as `+52rem` against `52rem`. Each spelling receives its own definition with identical meaning; harmless duplication is preferred over numeric parsing machinery.
7. An unsafe Tailwind entry keeps arbitrary variants, so output readability can differ across packages while rendered behavior remains preserved.
8. Packages in one entry group share Tailwind entry planning and loading fate, although `--force` may still skip an independent package-specific input failure.
9. Automatic shared-entry coverage is intentionally narrow: ancestry supplies candidates, a direct reference supplies loading proof, and a provable source scope supplies scan proof, with both proofs required.

## Deferred Work

- Generating `--breakpoint-*` theme variables for unmatched minimum-width bounds.
- Aliasing generated definitions to authored custom variants with a proven effective-expansion match.
- Duplicating consumer candidates per branch to decompose comma lists and `or` conditions.
- Explicit package-to-entry mappings for sibling or otherwise non-ancestor workspace consumers.
- Explicit mappings that vouch for consumers exposed through package export entry points.
- User-provided aliases for generated media definitions.
- Semantic deduplication that requires calculation or unit conversion.
- Extraction for HTML `<link media>` conditions.
- Named extraction for `@supports`, `@container`, and other conditional at-rules.
- Consolidating previously appended generated blocks.
- Moving generated definitions into a project-selected imported theme file.
