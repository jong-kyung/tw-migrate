# RFC: Media Query Definition Extraction

## Status

Proposed

## Summary

`tw-migrate` currently converts media queries to Tailwind built-in variants when their conditions match known media features or existing theme breakpoints. Other representable media queries become arbitrary variants such as `[@media_screen_and_(width_<=_768px)]:m-0`.

Arbitrary variants preserve behavior, but repeated query text makes migrated class values hard to read. This RFC gives unmatched media queries stable names in the project's Tailwind CSS entry:

- a simple unmatched minimum-width query becomes a generated `--breakpoint-*` theme variable;
- every other unmatched, representable media query becomes a generated `@custom-variant`;
- Tailwind built-ins and existing project breakpoints remain preferred;
- workspace packages without their own entry may share the nearest ancestor package's unique entry;
- extraction runs by default and has an opt-out CLI and API option; and
- an entry that cannot be edited safely keeps the current arbitrary-variant behavior.

The migration writes only definitions used by the final validated plan. Tailwind entry edits join the existing snapshot verification and atomic write transaction.

This RFC supersedes the unmatched `@media` behavior in the core RFC's **Breakpoints and At-Rules** section. It does not change `@supports`, `@container`, or `@starting-style` handling.

## Goals

1. Replace unreadable arbitrary media variants with stable named variants.
2. Define each normalized media condition once per Tailwind entry.
3. Reuse Tailwind built-ins and existing theme breakpoints before generating definitions.
4. Generate theme breakpoints only for media queries that match Tailwind's minimum-width model and active breakpoint units exactly.
5. Preserve complete max-width, range, media-type, and compound conditions through `@custom-variant`.
6. Keep generated names deterministic and derived from conditions rather than inferred device labels.
7. Include Tailwind entry changes in dry-run previews, integrity checks, and transactional writes.
8. Preserve the current arbitrary variant as the safe fallback when definition extraction is disabled or unavailable.
9. Keep a second migration run byte-identical.
10. Resolve and update one ancestor-owned Tailwind entry for every workspace package that shares it.

## Non-Goals

1. Inferring semantic names such as `mobile`, `tablet`, or `desktop` from width values.
2. Converting between `px`, `rem`, `em`, or other units.
3. Replacing existing project breakpoint names with generated names.
4. Replacing Tailwind built-in variants such as `dark`, `print`, `motion-reduce`, or `portrait`.
5. Extracting `@supports`, `@container`, `@starting-style`, or selector arbitrary variants.
6. Expanding HTML `<link media>` support in the first implementation.
7. Moving generated definitions into imported theme files or reorganizing existing `@theme` blocks.
8. Editing dependency-owned or symlinked Tailwind entries.
9. Adding a user-configurable naming template in the first implementation.
10. Inferring that a sibling package's Tailwind entry consumes another package.
11. Reusing an ancestor Tailwind entry outside `--workspaces` mode.

## Terminology

- **Existing variant**: a Tailwind built-in variant or a variant backed by the project's existing theme tokens.
- **Simple minimum-width query**: one width lower bound with no media type, modifier, list, upper bound, or additional feature.
- **Generated breakpoint**: a new `--breakpoint-*` variable created for a simple minimum-width query.
- **Generated custom variant**: a new `@custom-variant` that wraps the complete normalized media query.
- **Media definition**: either generated form.
- **Condition key**: the deterministic normalized representation used for deduplication and naming.
- **Extraction fallback**: use of the current arbitrary at-rule variant instead of changing the Tailwind entry.
- **Ancestor-shared entry**: the unique Tailwind entry owned by the nearest ancestor package of a workspace package that has no entry of its own.
- **Entry group**: selected workspace packages that resolve to the same Tailwind entry and therefore share one ordered entry plan.

## User-Visible Behavior

### Resolution order

For each `@media` condition, the planner tries these forms in order:

1. an existing Tailwind built-in media variant;
2. an exact existing project breakpoint or breakpoint range;
3. a generated breakpoint for a simple minimum-width query;
4. a generated custom variant for any other media query that can be represented safely;
5. the existing arbitrary at-rule variant when extraction is disabled or the Tailwind entry cannot be edited safely; and
6. existing retention behavior when neither a named nor arbitrary variant can preserve the condition.

The first two steps keep current output stable. The planner does not create aliases for conditions that already have project-defined names.

### Generated breakpoint

Given an unmatched simple query:

```css
@media (min-width: 52rem) {
  .card {
    padding: 2rem;
  }
}
```

The Tailwind entry gains:

```css
@theme {
  --breakpoint-min-52rem: 52rem;
}
```

A migrated consumer uses `min-52rem:p-8`.

The modern range form `(width >= 52rem)` has the same lower-bound meaning and may use the same condition key. A query such as `screen and (min-width: 52rem)` is not simple because removing `screen` would change the authored condition; it becomes a custom variant instead.

### Generated custom variant

Given:

```css
@media screen and (width <= 768px) {
  .card {
    margin: 0;
  }
}
```

The Tailwind entry gains:

```css
@custom-variant screen-width-lte-768px {
  @media screen and (width <= 768px) {
    @slot;
  }
}
```

A migrated consumer uses `screen-width-lte-768px:m-0`.

The custom variant preserves `screen` and the inclusive `<=` boundary. Converting this query to a Tailwind `max-*` breakpoint variant would change the boundary semantics, so the planner must not do that.

### Single-use conditions

Extraction does not require repetition. Every unmatched media condition receives a named definition when the entry is writable and the final plan uses that definition. Identical condition keys share one definition.

This policy favors readable migrated class values. It accepts that a Tailwind entry may contain a definition used by one migrated class.

### Workspace shared entries

Before package planning, workspace discovery builds a catalog of Tailwind entries owned by each package. Entry resolution follows these rules:

1. use the package's own unique entry when it has one;
2. in `--workspaces` mode, when the package has no entry, walk ancestor package roots from nearest to farthest;
3. continue past an ancestor with no entry;
4. select the first ancestor that owns exactly one entry;
5. fail the package as ambiguous when the nearest ancestor with entries owns more than one; and
6. never select an entry owned by a sibling or descendant package.

Default package mode retains the current ownership boundary and does not modify an ancestor entry. A package without an owned entry still fails entry resolution unless `--workspaces` enables the ancestor rule.

The entry owner supplies the project root used to resolve Tailwind, imported stylesheets, plugins, and theme configuration. The migrated child package continues to supply its own stylesheets, consumers, preprocessors, and failure attribution.

Packages that resolve to the same entry form one entry group. The group processes package plans in normalized package-path order and produces one Tailwind entry edit.

## Query Classification

A simple minimum-width query must meet all of these conditions:

1. it contains one width lower bound expressed as `min-width` or `width >=`;
2. the value is a finite non-negative CSS dimension accepted by Tailwind;
3. its unit matches the active project breakpoint namespace;
4. it has no media type or modifier;
5. it has no upper bound, comma-separated alternative, or additional feature; and
6. it does not depend on a custom media reference.

Tailwind warns against mixing breakpoint units because incomparable values can produce unexpected responsive utility ordering. For example, `(min-width: 768px)` becomes a custom variant in a project whose active breakpoints use `rem`; the planner does not convert `768px` to `48rem`. If the project already defines an exact `768px` breakpoint, the earlier existing-breakpoint step reuses it. When the active breakpoint namespace already mixes units or has no provable common unit, unmatched lower bounds use custom variants.

All other parseable media queries use a custom variant when the complete condition can be emitted inside `@custom-variant` without changing its meaning.

Nested `@media` rules keep their nesting order through stacked variants. The planner assigns each distinct condition its own definition rather than combining nested conditions into a new synthetic query.

## Normalization and Deduplication

The planner derives a condition key from the parsed media condition. Normalization may:

- remove comments and insignificant whitespace;
- case-fold CSS keywords and media feature names where CSS defines them as case-insensitive; and
- normalize legacy `min-width` and `max-width` forms to their exact inclusive range equivalents.

Normalization must not:

- convert units;
- assume a root font size;
- reorder `and` or comma-separated branches;
- simplify calculations;
- drop media types or modifiers; or
- merge conditions whose equivalence the parser cannot prove.

For example, `48rem` and `768px` remain different keys even when a typical browser configuration would render them at the same width.

Deduplication happens per resolved Tailwind entry. Packages in one entry group share one definition set even when their source stylesheets have different owners. Packages with separate entries receive separate definitions.

## Generated Names

### Breakpoint names

A generated breakpoint uses `min-<value>` after the `breakpoint-` namespace:

| Value     | Theme variable             | Variant        |
| --------- | -------------------------- | -------------- |
| `52rem`   | `--breakpoint-min-52rem`   | `min-52rem:`   |
| `47.5rem` | `--breakpoint-min-47p5rem` | `min-47p5rem:` |
| `768px`   | `--breakpoint-min-768px`   | `min-768px:`   |

The numeric form is canonical, the unit is lowercase, and `p` encodes a decimal point in the identifier.

### Custom variant names

A generated custom variant uses recognized media types, feature names, comparison operators, and values. Operators use stable identifiers such as `lt`, `lte`, `gt`, and `gte`.

Examples:

| Condition                            | Variant name                   |
| ------------------------------------ | ------------------------------ |
| `screen and (width <= 768px)`        | `screen-width-lte-768px`       |
| `(48rem <= width < 60rem)`           | `width-gte-48rem-lt-60rem`     |
| `(hover: hover) and (pointer: fine)` | `hover-hover-and-pointer-fine` |

Names must be valid Tailwind variant identifiers. If a descriptive name would exceed the implementation's fixed length limit or collide with a different definition, the planner keeps the readable prefix and appends a short stable digest of the condition key.

The planner reuses an existing definition only when its normalized meaning matches. It never overwrites or renames an authored breakpoint or custom variant.

## Tailwind Entry Editing

The resolved Tailwind CSS entry becomes a planned source file only when the final migration uses at least one missing media definition.

The editor follows these rules:

1. keep every existing byte unchanged;
2. append missing definitions at the top level after existing content;
3. group missing breakpoint variables in one new `@theme` block;
4. emit each missing `@custom-variant` as its own block;
5. sort breakpoints and custom variants by generated name;
6. add no definition that lacks a final validated candidate; and
7. recognize existing matching definitions so a second run produces no diff.

The first implementation appends a new block rather than inserting declarations into an authored `@theme` block. A later run may append another block when it discovers a new condition. Tailwind supports multiple `@theme` blocks, and this policy avoids reparsing and reprinting user-owned formatting.

An entry group owns one mutable in-memory entry source initialized from the shared snapshot. Each package plan receives the current source and may return a new source containing moved keyframes or global at-rules. The orchestration layer adopts that planned source before adding media definitions or processing the next package. It removes intermediate Tailwind entry files from package plans and emits only the group's final composed entry file. Media extraction must never add a second planned file for a path already changed by the native planner.

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

The option composes with positional stylesheet migration, package migration, `--workspaces`, `--force`, and `--dry-run`. In workspace mode, ancestor entry inheritance is automatic and does not add a package-to-entry mapping option. A dry run includes each changed owned or ancestor-shared Tailwind entry in `changedFiles` and the unified diff but does not write it.

## Planning Architecture

### Native planner

The Rust planner performs one package-wide media-condition prepass before producing consumer edits. The prepass:

1. classifies each condition using the resolution order;
2. derives condition keys and deterministic names;
3. deduplicates definitions for the resolved entry; and
4. associates generated candidates with the definitions they require.

The internal native response adds media definitions and candidate-to-definition references. These fields are orchestration data and do not change `MigrationReport`.

### TypeScript orchestration

The TypeScript layer:

1. discovers Tailwind entries for every package before calling `planPackage()`;
2. resolves owned and ancestor-shared entries and records each entry's owning package;
3. groups selected packages by resolved entry;
4. rejects unsafe entry groups before enabling extraction;
5. processes each group in normalized package-path order while carrying one mutable entry source;
6. resolves Tailwind and its import graph from the entry owner's package root;
7. folds each native planner entry result into the group's current source;
8. aggregates media definitions for the group and builds the augmented source in memory;
9. validates every generated candidate against the augmented design system;
10. removes definitions unused after compile-failure replanning; and
11. adds one final entry edit per group to the merged transaction.

Intermediate package plans must not claim the shared entry. The final group edit is the only Tailwind entry file passed to `mergePlans()`, which continues to reject accidental duplicate path claims.

Package-local Sass, Less, and Vue compiler loading remains rooted at the migrated package. Tailwind loading uses the entry owner because that package owns the entry's imports, plugins, dependency resolution, and theme. No bundled Tailwind compiler or new dependency is introduced.

### Candidate validation and replanning

Generated named variants must compile before any file is written. If a generated media definition cannot compile but the equivalent arbitrary variant compiles, the planner replans that condition with the arbitrary form. If neither form compiles, existing `candidate-compilation-failure` retention applies.

The augmented entry itself must parse and load successfully. A failure caused by generated syntax is a fatal planner error because writing a partially valid design-system entry would violate the transaction contract.

## Safety and Fallbacks

The planner uses arbitrary variants when extraction is enabled but the resolved Tailwind entry is known before planning to be unsafe to edit, including when it is:

- a symbolic link;
- outside the writable project scope; or
- shared with an unselected package in a way that prevents one complete transaction.

The migration emits one `media-query-definition-fallback` warning per affected Tailwind entry. The warning uses `(0, 0)` offsets and reports that behavior was preserved with arbitrary variants because the entry was not edited.

The fallback does not weaken existing integrity rules:

- a source change after snapshotting remains fatal;
- a write or rollback failure remains fatal;
- `--force` does not hide integrity or transaction failures; and
- file permissions remain preserved.

If extraction is disabled explicitly, arbitrary variants are expected behavior and produce no fallback warning.

A recoverable failure in one child package may be skipped under `--force` without discarding successful siblings in the same entry group. Entry discovery, Tailwind loading, or augmented-entry validation failures affect every package that depends on that shared entry and therefore skip the complete entry group only when the existing `--force` policy classifies the failure as recoverable. Integrity and write failures remain fatal for the full migration.

## Determinism and Idempotency

Condition keys, generated names, definitions, candidates, warnings, and entry edits use stable sorting. Naming does not depend on discovery order or hash-map iteration.

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

### Phase 1: Classification and naming

- Add media condition keys and the built-in, existing-breakpoint, generated-breakpoint, and custom-variant resolution order.
- Add deterministic breakpoint and custom variant names.
- Return internal media definition metadata from batch planning.
- Keep extraction disabled internally until entry augmentation is available.

### Phase 2: Entry augmentation and options

- Add the CLI opt-out and public API option.
- Discover owned entries before package planning and resolve nearest-ancestor entries in workspace mode.
- Group packages by entry and resolve Tailwind from the entry owner.
- Carry one entry source through ordered package plans, preserving moved keyframes and global at-rules.
- Aggregate media definitions per entry group.
- Load and validate an augmented design system in memory.
- Add one final Tailwind entry file per group to snapshot verification and the atomic write transaction.
- Add arbitrary fallback and its warning for entries that cannot be edited safely.

### Phase 3: Packaged and browser coverage

- Add packaged CLI snapshots for preview, write, opt-out, fallback, and second-run behavior.
- Add controlled browser cases at, below, and above generated boundaries.
- Cover separate and shared workspace entries.

Public documentation describes the feature only after all three phases land.

## Testing Strategy

### Rust planner tests

- existing built-in media variants remain unchanged;
- exact existing breakpoints remain unchanged;
- unmatched simple lower bounds with compatible units produce generated breakpoints;
- unmatched simple lower bounds with incompatible units produce custom variants;
- media types, max bounds, ranges, and compound conditions produce custom variants;
- inclusive and exclusive operators receive distinct names and preserve their conditions;
- decimal values produce valid stable identifiers;
- duplicate condition keys share one definition;
- unit differences do not deduplicate;
- name collisions receive stable digest suffixes; and
- extraction-disabled planning reproduces current arbitrary candidates.

### Node and API tests

- the resolved Tailwind entry receives only definitions used by final candidates;
- augmented candidates compile with the target project's Tailwind v4 installation;
- `extractMediaQueries: false` leaves the entry unchanged;
- dry-run reports the entry diff without writing;
- write mode changes the entry and consumers in one transaction;
- source changes and write failures preserve current fatal behavior;
- symlinked or out-of-scope entries use arbitrary variants and warn;
- a package-owned entry wins over an ancestor entry;
- a package without an entry inherits the nearest ancestor's unique entry only in workspace mode;
- an ancestor with multiple entries fails the child package deterministically;
- sibling entries are never inferred;
- Tailwind loads from the entry owner while preprocessors load from each migrated package;
- shared workspace entries receive one merged edit that preserves native keyframe and global at-rule additions; and
- a second run produces no diff.

### Packaged snapshots

Snapshots cover:

- CLI help for `--no-extract-media-queries`;
- generated breakpoint and custom variant output;
- unchanged built-in and existing breakpoint output;
- deterministic definition ordering;
- fallback diagnostics; and
- exact workspace deltas for preview and write modes.

### Browser ecosystem tests

A controlled fixture captures computed styles:

- below, at, and above an inclusive max-width boundary;
- below and above a generated minimum-width breakpoint; and
- under a compound media query represented by a custom variant.

The pre- and post-migration captures must match.

## Success Criteria

1. Existing built-in and project-defined breakpoint output does not change.
2. An unmatched simple minimum-width query with a compatible unit uses a generated named breakpoint.
3. Every other safely representable unmatched media query uses a generated custom variant when the entry is writable.
4. Generated definitions preserve media types, operators, values, and compound structure.
5. No unit conversion or device-name inference occurs.
6. Every named candidate compiles against the augmented target-project design system.
7. Dry-run and write mode use the same final plan.
8. Unsafe entry scope falls back to behavior-preserving arbitrary variants.
9. Tailwind entry and consumer edits commit or roll back together.
10. Repeated conditions share one definition, shared workspace entries receive one edit, and a second run is a no-op.
11. A workspace package without an entry inherits only the nearest ancestor package's unique entry.
12. Entry groups preserve all package-produced keyframes, global at-rules, and media definitions in one composed source.

## Accepted Trade-offs

1. A single-use unmatched query adds a Tailwind definition. Readable consumer classes take priority over minimizing entry length.
2. Generated names describe conditions rather than product semantics. `screen-width-lte-768px` is longer than `tablet`, but it does not invent a design-system meaning.
3. Appending a new `@theme` block can leave multiple theme blocks after migrations at different times. This preserves authored formatting and avoids owning a managed section.
4. Conditions that differ only through unit conversion remain separate even when they render similarly in one environment.
5. An unsafe Tailwind entry keeps arbitrary variants, so output readability can differ across packages while rendered behavior remains preserved.
6. Packages in one entry group share Tailwind entry planning and loading fate, although `--force` may still skip an independent package-specific input failure.

## Deferred Work

- Explicit package-to-entry mappings for sibling or otherwise non-ancestor workspace consumers.
- User-provided aliases for generated media definitions.
- Semantic deduplication that requires calculation or unit conversion.
- Extraction for HTML `<link media>` conditions.
- Named extraction for `@supports`, `@container`, and other conditional at-rules.
- Consolidating previously appended generated blocks.
- Moving generated definitions into a project-selected imported theme file.
