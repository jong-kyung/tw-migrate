# RFC: CSS-to-Tailwind Migration CLI

## Status

Proposed

## Background

Migrating an existing React or Next.js project from authored CSS to Tailwind is usually a manual process:

1. Find every JSX element affected by a CSS selector.
2. Translate each declaration into a Tailwind utility.
3. Preserve selector conditions such as pseudo-classes, descendants, and media queries.
4. Update `className` without rewriting unrelated source formatting.
5. Remove CSS Module rules only when their exports are no longer needed.

Text search is not sufficient for this work. CSS Modules rename selectors at build time, JSX expressions have multiple static forms, selectors may span component boundaries, and Tailwind class order does not directly determine CSS precedence.

This RFC proposes `tw-migrate`, a one-shot codemod distributed as an npm package. It parses CSS and React source in Rust, exposes the migration through NAPI-RS, and uses the target project's Tailwind v4 installation to validate generated utilities.

## Goals

1. Migrate one selected CSS file at a time.
2. Support React and Next.js JavaScript/TypeScript source containing JSX.
3. Resolve static global class/id references and CSS Module imports.
4. Prefer exact Tailwind theme utilities and fall back to arbitrary values or properties.
5. Preserve unrelated source formatting through span-based edits.
6. Preview all changes by default and write only with an explicit flag.
7. Expose the same operation through a high-level Node.js API.
8. Publish prebuilt native packages for the main macOS, Linux, and Windows targets.

## Non-Goals

1. Vue, Svelte, Angular, or template languages other than JSX/TSX.
2. Tailwind v3 or earlier.
3. Dynamic or conditional `className` evaluation.
4. Runtime DOM inspection or browser-based migration.
5. A persistent linter, watcher, or CI daemon.
6. Automatic deletion of global CSS rules.
7. Complete React rendering analysis across arbitrary higher-order components, portals, or runtime component selection.
8. SCSS, Sass, or Less migration in the first release.
9. Tailwind's unstable design-system APIs.

## User Experience

### CLI

```bash
# Preview a migration and print its diff.
npx tw-migrate src/components/Button.module.css

# Apply the previewed migration.
npx tw-migrate src/components/Button.module.css --write

# Select a Tailwind entry when auto-detection is ambiguous.
npx tw-migrate src/components/Button.module.css \
  --tailwind-css src/app/globals.css \
  --write
```

The command accepts exactly one positional CSS file in the first release. Directory and glob migration are deferred until single-file behavior is reliable.

Preview output includes:

- affected source files;
- a unified diff;
- converted and retained CSS rules;
- generated Tailwind candidates;
- warnings with source locations and reasons.

### Node.js API

The package exposes one public operation:

```ts
interface MigrateOptions {
  cssFile: string;
  cwd?: string;
  write?: boolean;
  tailwindCss?: string;
}

interface MigrationReport {
  changedFiles: string[];
  diff: string;
  convertedRules: number;
  retainedRules: number;
  warnings: MigrationWarning[];
}

export function migrate(options: MigrateOptions): Promise<MigrationReport>;
```

Parser ASTs, edit planners, and Tailwind mapping internals are not public API.

## Supported Projects

- Node.js 20 or newer.
- Tailwind v4 installed in the target project.
- JavaScript or TypeScript source using React JSX syntax.
- Plain `.css` and `.module.css` input files.

The source scanner includes `.js`, `.jsx`, `.ts`, and `.tsx` files and excludes generated/vendor directories such as `node_modules`, `.next`, `dist`, and `build`.

## Architecture

```text
┌─────────────────────────────────────────────────────────────────┐
│ npm package                                                     │
│                                                                 │
│  CLI / public migrate()                                         │
│    ├─ resolve project and Tailwind v4 installation              │
│    ├─ locate the Tailwind CSS entry                             │
│    ├─ call the NAPI-RS native core                              │
│    ├─ validate candidates with Tailwind compile()               │
│    └─ print or apply the migration report                       │
└──────────────────────────────┬──────────────────────────────────┘
                               │ NAPI
┌──────────────────────────────▼──────────────────────────────────┐
│ Rust native core                                                │
│                                                                 │
│  Oxc CSS parser                                                  │
│    └─ rules, selectors, declarations, and byte spans            │
│                                                                 │
│  Oxc JS/TS parser + semantic analysis                           │
│    └─ imports, bindings, JSX attributes, and component graph    │
│                                                                 │
│  Migration planner                                              │
│    ├─ selector-to-element matching                              │
│    ├─ CSS-to-utility mapping                                    │
│    ├─ conflict diagnostics                                      │
│    └─ non-overlapping span edits                                │
└─────────────────────────────────────────────────────────────────┘
```

### Why NAPI-RS

The product requires both a CLI and a Node.js API. NAPI-RS lets both surfaces reuse the Rust parser and migration core while preserving normal npm installation and `npx`/`pnpx` execution.

The npm package's JavaScript layer remains responsible for loading the target project's Tailwind package because Tailwind configuration and compilation are native Node.js concerns.

### Why Oxc

Both CSS and JSX/TSX analysis require byte spans for minimal source edits. Oxc provides span-bearing ASTs and semantic information needed to distinguish bindings with the same name and trace supported component paths.

`ast-grep` is not included in the first release because Oxc already supplies the required parsing, binding, and span data. Using both would duplicate parsing without removing the need for custom semantic analysis.

## Migration Pipeline

### 1. Resolve the Project

The CLI resolves the project from `cwd`, verifies Node.js and Tailwind requirements, and normalizes the selected CSS path.

A parse or configuration error is fatal before any file is written.

### 2. Resolve the Tailwind Entry

The JavaScript layer searches for CSS entries that load Tailwind, for example:

```css
@import "tailwindcss";
```

If exactly one entry is found, it is selected automatically. If multiple entries are found, migration stops and requires `--tailwind-css`.

The project-installed Tailwind v4 compiler is used. The tool does not bundle a separate Tailwind version and does not call `__unstable__loadDesignSystem`.

### 3. Parse the Selected CSS

The Rust core records:

- selector AST and source span;
- declaration names, values, importance, and spans;
- containing media rules;
- CSS Module local classes and ids;
- selector lists and nested conditions.

Unsupported at-rules remain unchanged and produce warnings when they block migration.

### 4. Discover Source References

#### Global CSS

Static `className` and `id` values are matched across project source files.

#### CSS Modules

The scanner follows imports of the selected file:

```tsx
import styles from './Button.module.css';

export function Button() {
  return <button className={styles.button}>Save</button>;
}
```

Bindings are resolved semantically, so another `styles` variable or another module with the same local class name does not match.

### 5. Match Selectors to Elements

#### Global selectors

A selector may migrate when:

1. Tailwind can express its condition as a built-in or arbitrary variant; and
2. the target JSX nodes can be found statically.

For example:

```css
.parent > .child:hover {
  color: red;
}
```

may add an equivalent variant utility to statically resolved `.child` elements.

Global rules remain in the source CSS after migration. The report marks them as manual cleanup candidates.

#### CSS Module selectors

CSS Module local names are hashed during the project build. A literal arbitrary variant such as `[.parent_&]` would not match the generated parent class.

The migration therefore converts a complex CSS Module selector only when its relationship can be proven statically. Supported component traversal is intentionally conservative:

- project-local function components;
- a single statically analyzable JSX return;
- direct JSX ancestry;
- direct `children` or prop forwarding.

The following are not inferred:

- conditional render branches;
- portals;
- higher-order components;
- dynamic component variables;
- arbitrary prop transformations;
- runtime-generated element trees.

No generated marker classes are introduced. Unproven selectors remain in CSS and produce a warning.

### 6. Read Supported `className` Forms

The first release supports:

```tsx
<div className="card featured" />
<div className={styles.card} />
<div className={`${styles.card} featured`} />
```

A template literal is supported only when every substitution is a statically resolved CSS Module member or string.

The following forms are skipped:

```tsx
<div className={active ? 'active' : 'inactive'} />
<div className={clsx(styles.card, active && styles.active)} />
<div className={cn(styles.card, props.className)} />
```

A skipped use prevents automatic CSS Module export deletion when that export may still be needed.

### 7. Convert Declarations

Conversion follows this order:

1. Normalize a supported declaration group to its final effective longhands.
2. Find an idiomatic Tailwind utility whose theme value exactly matches.
3. Validate the candidate with the project's Tailwind compiler.
4. If no exact idiomatic utility exists, generate an arbitrary value or property.
5. Validate the fallback candidate.
6. Retain the declaration and warn if neither candidate compiles safely.

Examples:

```css
padding: 1rem;
/* Exact project token → p-4 */

padding: 13px;
/* No exact token → p-[13px] */

scrollbar-color: red blue;
/* No idiomatic mapper → [scrollbar-color:red_blue] */
```

No nearest-token approximation is allowed because it would change rendered values.

### Shorthand and Longhand Overlap

CSS declaration order can carry semantics that Tailwind class string order cannot preserve:

```css
.card {
  margin: 1rem;
  margin-left: 2rem;
}
```

For explicitly supported mapper families, the core computes the final longhand values before selecting utilities. If an overlap cannot be normalized safely, the affected rule remains in CSS and produces a warning.

### Existing Tailwind Conflicts

If an element already has a Tailwind utility affecting the same property, the new candidate is appended and a warning is emitted:

```tsx
// Before
<div className="p-2" />

// Proposed migration
<div className="p-2 p-4" />
```

The tool does not assume that `p-4` wins because class attribute order does not determine Tailwind stylesheet order.

Per the selected migration policy, a fully converted CSS Module rule may still be deleted after this warning. This is an explicitly accepted risk: the resulting style may differ when existing and generated utilities conflict.

Exact duplicate candidates are not appended, keeping migration idempotent.

### Breakpoints and At-Rules

A media query migrates when its minimum-width condition exactly matches a Tailwind theme breakpoint:

```css
@media (min-width: 768px) {
  .card {
    padding: 2rem;
  }
}
```

may become `md:p-8` when `md` is exactly `768px` in the target theme. A bounded range such as `@media (min-width: 48rem) and (max-width: 63.999rem)` becomes `md:max-lg:p-8` when the bounds match the `md` and `lg` breakpoints. Queries with additional or unmatched conditions remain unchanged.

Supported `@media`, `@supports`, `@container`, and `@starting-style` blocks are traversed recursively and become stacked variants such as `motion-reduce:starting:@md:grid`. Media features map only to equivalent Tailwind variants, and unnamed minimum-width container queries use exact theme tokens or arbitrary container values. If a nested condition or statement cannot be represented safely, its outer conditional block and related CSS Module classes remain unchanged.

Local CSS Module `@keyframes` referenced by a single static `animation` or `animation-name` are renamed deterministically and moved to the Tailwind entry before the module is removed. Global definition at-rules such as `@font-face`, `@property`, `@counter-style`, and `@view-transition` are also moved before removal when they contain no URL dependency. Multiple animations, ambiguous names, relative URLs, dynamic values, and structural at-rules remain unchanged.

## CSS Rule Cleanup

### Global CSS

Global rules are never removed automatically. Their classes and ids may be used by runtime HTML, CMS content, unscanned files, or external code.

### CSS Modules

A CSS Module rule and its JSX module reference may be removed only when:

1. every declaration in the rule was converted and validated;
2. every known use of the export is a supported `className` use;
3. no alias or non-`className` JavaScript use exists;
4. the export is not referenced by `composes`;
5. every required selector relationship was proven.

Unsafe examples include:

```ts
container.querySelector(`.${styles.card}`);
const cardClass = styles.card;
```

```css
.featured {
  composes: card;
}
```

When an export is removed, its JSX member reference is removed as well. An import is deleted only when no imported CSS Module members remain in use. A CSS Module file is deleted after every rule and movable at-rule dependency has been migrated; otherwise the file and required imports remain.

For selector lists, the entire rule is removed only when every selector is safely migrated. The first release does not split partially migrated declaration blocks.

## Source Editing

Parsing and printing an entire AST can change unrelated quotes, whitespace, semicolons, or line wrapping. The migration instead uses AST spans to produce non-overlapping text edits.

Examples of localized edits include:

- append utilities inside an existing string literal;
- replace a CSS Module member with a static utility string;
- add a new `className` attribute;
- remove an unused CSS Module import;
- remove a fully migrated CSS rule.

Before writing:

1. all edits are calculated in memory;
2. overlapping edits are rejected;
3. edited CSS and source files are reparsed with Oxc;
4. every generated Tailwind candidate is compiled;
5. files are written through temporary files and renamed.

No write occurs when preflight validation fails.

## Diagnostics

Warnings are non-fatal and include a stable reason code, file, span, and message. Initial warning categories include:

- dynamic `className`;
- unresolved selector target;
- unproven CSS Module relationship;
- unsupported at-rule;
- unnormalizable shorthand/longhand overlap;
- existing Tailwind property conflict;
- non-`className` CSS Module reference;
- Tailwind candidate compilation failure;
- retained global rule.

Fatal errors include invalid input CSS, invalid source produced by an edit, missing Tailwind v4, ambiguous Tailwind entry without an override, and filesystem write failures.

## Packaging and Distribution

The main npm package contains the CLI and JavaScript orchestration layer. NAPI-RS publishes platform-specific optional dependencies containing the native addon.

Initial targets:

| Platform | Architectures |
| --- | --- |
| macOS | arm64, x64 |
| Linux glibc | arm64, x64 |
| Windows | x64 |

Linux musl and Windows arm64 are follow-ups rather than first-release requirements.

The current `oxc-css-parser` dependency requires Rust 1.95, which becomes the initial Rust toolchain floor unless the dependency changes before implementation.

## Testing Strategy

The migration is tested before application builds. Browser and full Next.js builds are not release requirements for the first version.

Each fixture asserts:

1. the exact preview diff;
2. the exact warning report;
3. successful Oxc reparsing of every edited file;
4. successful Tailwind v4 compilation of every generated candidate;
5. a zero-diff second migration run.

Fixture groups include:

- global CSS classes and ids;
- CSS Modules and import aliasing;
- JSX and TSX syntax;
- static string, member, and template-literal class names;
- pseudo-classes and arbitrary variants;
- direct JSX and supported component paths;
- exact theme tokens and arbitrary fallbacks;
- exact breakpoints;
- shorthand/longhand normalization;
- existing Tailwind conflicts;
- `composes` and non-`className` references;
- unsupported selectors and at-rules;
- malformed CSS and source files.

Fixtures use multiple representative React and Next.js directory layouts without requiring those applications to build.

## Implementation Phases

### Phase 1: Package and Parser Foundation

- Set up the npm package, NAPI-RS crate, CLI entry, and Node 20 engine requirement.
- Parse CSS and JSX/TSX with Oxc.
- Produce a read-only report for selector and reference discovery.

### Phase 2: Tailwind Bridge and Declaration Mapping

- Resolve a project's Tailwind v4 package and CSS entry.
- Implement stable `compile()` validation.
- Add exact-token mapping and arbitrary fallback.
- Add effective-longhand normalization for the initial mapper families.

### Phase 3: Selector and Component Analysis

- Match global selectors to static elements.
- Resolve CSS Module imports and exports.
- Add conservative direct component traversal.
- Generate variants for validated selector conditions and exact breakpoints.

### Phase 4: Editing and Cleanup

- Generate span-based JSX and CSS edits.
- Remove eligible CSS Module rules, references, and imports.
- Add preview diff and `--write` preflight.

### Phase 5: Distribution

- Build and publish native artifacts for the initial platform matrix.
- Run the full fixture suite for each native target.
- Validate npm, npx, and pnpx installation paths.

## Success Criteria

1. `tw-migrate <file>` produces a deterministic preview without modifying files.
2. `--write` applies only a fully validated edit plan.
3. Supported static global and CSS Module references are found without name collisions.
4. Every generated utility compiles with the target project's Tailwind v4 installation.
5. Exact theme values are never replaced by approximate tokens.
6. Unrelated source formatting remains byte-for-byte unchanged.
7. A second run produces no diff.
8. Unsupported or ambiguous behavior is retained and reported rather than silently guessed.
9. The Node API and CLI produce the same migration report.
10. The npm package installs and runs on the initial platform matrix.

## Accepted Trade-offs

1. Global CSS remains after utilities are added, so global migrations require manual cleanup.
2. Dynamic `className` patterns reduce conversion coverage but avoid runtime guesses.
3. CSS Module complex selectors may remain when static component analysis cannot prove the DOM relationship.
4. Existing utility conflicts are appended with warnings, and converted CSS Module rules may still be deleted; this can change styling.
5. Arbitrary properties provide broad correctness before every declaration family has an idiomatic mapper.
6. Single-file migration avoids batch transaction and failure-isolation complexity in the first release.

## Follow-ups

- Tailwind v3 support.
- Directory and glob migration.
- `clsx`, `classnames`, and known `cn` helper analysis.
- Additional media, supports, and container-query variants.
- Broader React component-flow analysis.
- Linux musl and additional Windows targets.
- Optional visual regression fixtures if structural validation proves insufficient.
