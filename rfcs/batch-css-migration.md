# RFC: Batch CSS Migration

## Status

Accepted

## Summary

`tw-migrate` currently requires one CSS file per invocation. This RFC extends it so an invocation without a CSS path discovers and migrates every eligible CSS file in the current package. A supplied CSS path continues to scope conversion to that file. Workspace-wide migration is explicit through `--workspaces`.

The batch operation reads the project once, builds one deterministic edit plan, validates the complete result, and applies it atomically. Independent per-CSS migrations are not written one after another because they can edit the same JavaScript or Tailwind entry from stale source.

## Goals

1. Migrate every eligible `.css` file in the current package when no CSS path is supplied.
2. Preserve single-file migration when a CSS path is supplied.
3. Find every supported JavaScript/TypeScript reference to each selected stylesheet.
4. Group migration by the nearest package boundary and that package's Tailwind v4 entry.
5. Support an explicit `--workspaces` mode for all packages below the workspace root.
6. Respect `.gitignore` during automatic discovery.
7. Produce one deterministic preview and one validated write transaction.
8. Retain only conflicting rules while continuing independent conversions.
9. Allow `--force` to skip failed package groups without weakening edit, Tailwind, or write validation.
10. Establish a measurable baseline before adding parallel parsing.

## Non-Goals

1. SCSS, Sass, and Less parsing or migration in this release.
2. Emotion or styled-components discovery.
3. Selecting migration scope from a JavaScript or TypeScript positional path.
4. Directory, glob, or include/exclude configuration beyond current package and workspace scope.
5. Guessing ownership across package boundaries when a package has no unique Tailwind entry.
6. Parallel analysis in the initial implementation.
7. Automatic deletion of global CSS rules.

Preprocessor extensions must not be treated as CSS merely because they are discovered. They will be added only with parsers and source-editing rules that preserve their original syntax.

## User Experience

### CLI

```bash
# Preview every eligible CSS file in the current package.
tw-migrate

# Preview one CSS file while scanning all relevant source references.
tw-migrate src/components/Button.module.css

# Apply the current package migration.
tw-migrate --write

# Apply every discovered workspace package.
tw-migrate --workspaces --write

# Skip recoverably failed package groups and apply successful groups.
tw-migrate --workspaces --write --force

# Resolve a package with an ambiguous Tailwind entry.
tw-migrate --tailwind-css src/app/globals.css --write
```

The command accepts at most one positional CSS file. `--workspaces` cannot be combined with a positional CSS file because the positional path already defines the requested scope.

Preview remains the default. `--force` has no effect on unsupported rules that are already represented as warnings; those rules remain unchanged in every mode.

### Node.js API

```ts
interface MigrateOptions {
  cssFile?: string;
  cwd?: string;
  write?: boolean;
  tailwindCss?: string;
  workspaces?: boolean;
  force?: boolean;
}
```

`migrate()` with no `cssFile` processes the current package. `workspaces: true` processes all package groups below the workspace root. Supplying both `cssFile` and `workspaces: true` is an error.

The existing aggregate report fields remain. Package failures skipped by `force` are returned separately:

```ts
interface MigrationFailure {
  package: string;
  message: string;
}

interface MigrationReport {
  changedFiles: string[];
  diff: string;
  convertedRules: number;
  retainedRules: number;
  rules: RuleReport[];
  candidates: string[];
  warnings: MigrationWarning[];
  failures: MigrationFailure[];
}
```

## Scope Discovery

### Package Boundaries

The current package is the nearest ancestor containing `package.json`. The workspace root is the Git root when available, otherwise the current package root.

In default mode only the current package contributes migration targets. In `--workspaces` mode, package directories below the workspace root are discovered from non-ignored `package.json` files. Every selected CSS file belongs to its nearest package boundary.

A package group must resolve exactly one Tailwind v4 CSS entry unless `tailwindCss` explicitly selects one for a single-package invocation. Tailwind entry files are inputs and possible edit destinations, never migration targets.

A package with no Tailwind entry or multiple entries fails before planning. In default mode this prevents every write. With `--force`, the package group is skipped.

### CSS Targets

Automatic discovery selects `.css` files within the requested package groups after excluding:

- Tailwind entry files;
- `.git`, `.next`, `build`, `dist`, and `node_modules`;
- paths ignored by Git.

A positional CSS path overrides ignore rules and selects exactly that file. It must end in `.css` and belong to the current package.

### Source References

Migration targets and writable sources are scoped separately:

- default mode writes CSS and supported JS/TS files in the current package;
- workspace mode writes files in all selected package groups;
- safety analysis scans supported JS/TS and CSS references throughout the workspace root.

If a selected CSS Module has a consumer outside the writable scope, the affected module rule and import are retained with a warning. The external consumer is not modified. This permits safe current-package migration without silently expanding its write scope.

## Batch Architecture

```text
resolve scope
  -> discover non-ignored files once
  -> snapshot CSS, JS/TS, package manifests, and Tailwind entries
  -> group targets by package and Tailwind entry
  -> plan every group against the immutable snapshot
  -> merge edits by absolute path and original byte span
  -> retain cross-stylesheet conflicts
  -> validate edited syntax and Tailwind candidates
  -> verify inputs still match the snapshot
  -> print preview or atomically write
```

### One Snapshot

Every relevant file is read once before planning. The planner never reads a source file after another CSS plan has modified it. All edits target the same immutable source snapshot.

Before writing, every destination is read again and compared with its snapshot. A concurrent user or tool edit aborts the operation instead of being overwritten.

### One Combined Plan

The native planner receives all CSS targets for a package group together. It resolves all CSS Module bindings and global selector candidates before producing JS edits. This avoids these unsafe patterns:

- two CSS files replacing the same `className` from different source versions;
- one CSS plan deleting an import needed by another stylesheet;
- multiple plans independently appending to the same Tailwind entry;
- the last file write silently discarding an earlier migration.

Edits are sorted deterministically by path and descending byte offset. Overlapping edits remaining after conflict handling are an internal planning error and abort the requested operation.

### Cross-Stylesheet Conflicts

If different selected stylesheets generate utilities that affect the same Tailwind property and variant on the same JSX element, class attribute order cannot preserve the original cascade.

The planner therefore:

1. identifies every contributing CSS rule;
2. retains those rules in their original stylesheets;
3. leaves their JS references unchanged;
4. emits a stable warning naming the conflicting candidates;
5. continues planning unrelated rules.

A retained conflict is a warning, not a package failure.

### Tailwind Validation

Each package group loads its Tailwind installation and design system once. All candidates in that group are validated against that group's prefix, theme, config, and plugins.

A candidate validation failure is an integrity failure, not a recoverable unsupported rule. It aborts the complete requested operation even with `--force` because it indicates that the generated edit plan cannot be trusted.

## Failure and Transaction Policy

### Default

Without `--force`, the requested scope is one transaction. Discovery, parsing, planning, candidate validation, source validation, or pre-write snapshot failure causes no file changes.

### Force

`--force` changes only package-group admission. A group with a recoverable discovery or parse failure is reported and excluded. Successful groups are then merged and written through one transaction.

`--force` never ignores:

- overlapping final edit ranges;
- invalid generated JavaScript, TypeScript, or CSS;
- invalid Tailwind candidates;
- changed inputs detected before commit;
- staging, rename, or rollback failures.

The package group is the minimum failure-isolation unit. CSS-file-level isolation is deferred because files in a group can share JS destinations and a Tailwind entry.

### Atomic Write

All final files are staged before any destination is replaced. Existing files are renamed to backups, staged files are installed, deletions remain represented by backups, and backups are removed only after the complete operation succeeds. A failure restores every available backup.

File permissions are preserved. Leftover staging or backup files continue to block a later run until the user restores or removes them.

## Git Ignore Behavior

Automatic discovery uses Git's ignore semantics when the workspace is a Git repository. Explicit CSS paths bypass ignore filtering. Outside Git, the fixed ignored-directory list still applies.

No new ignore-pattern parser or glob dependency is introduced for the initial implementation.

## Determinism

Package groups, CSS targets, source files, warnings, candidates, and output operations are sorted by normalized path and stable secondary keys. Repeated previews from the same snapshot must be byte-for-byte identical.

The preview and write paths consume the same final plan. A successful second invocation must produce no additional changes.

## Parallelism

Initial implementation is sequential after eliminating repeated project scans and repeated Tailwind loads. This provides the performance baseline and keeps the planner deterministic.

If benchmarks show CSS parsing is material, a later implementation may use a bounded worker pool for immutable CSS reads, parsing, and candidate extraction. Module resolution, edit merging, conflict handling, validation, and commit remain coordinated stages. Running complete per-CSS migrations concurrently is explicitly disallowed.

## Testing Strategy

### Discovery

- no positional path discovers all non-ignored CSS in the current package;
- a positional ignored CSS file is still processed;
- Tailwind entries and fixed generated directories are excluded;
- `--workspaces` discovers nested package groups;
- default mode does not write another package;
- workspace references outside writable scope prevent unsafe module cleanup.

### Grouping

- one package entry is selected automatically;
- missing and duplicate entries fail deterministically;
- workspace packages use their own Tailwind installations and entries;
- a shared package without a unique entry is skipped only with `--force`.

### Planning

- two CSS Modules can update different references in one JS file without lost edits;
- multiple selected rules can update one static `className`;
- cross-stylesheet utility conflicts retain only contributing rules;
- imports and CSS Module files are deleted only after all batch references are safe;
- candidates and warnings are deterministically ordered.

### Transactions

- preview writes nothing;
- default failure writes nothing;
- `--force` excludes a failed package group and applies successful groups;
- candidate or edit-integrity failure aborts even with `--force`;
- a source changed after planning aborts the write;
- an interrupted write restores prior content;
- a second run is a no-op.

## Implementation Sequence

1. Extend CLI and API options while preserving single-file behavior.
2. Add package-aware, Git-ignore-aware discovery and Tailwind grouping.
3. Replace the single-CSS native request with a package batch request.
4. Resolve all batch CSS Module and global candidates before planning source edits.
5. Add cross-stylesheet conflict retention and combined edit validation.
6. Aggregate package reports and implement `--force` package admission.
7. Add pre-write snapshot checks and retain the existing atomic writer.
8. Benchmark representative repositories before deciding whether to parallelize parsing.

## Success Criteria

1. `tw-migrate` previews every eligible CSS target in the current package.
2. `tw-migrate <file>` preserves exact single-file scope.
3. `--workspaces` processes package groups with their own Tailwind entries.
4. No batch edit can overwrite another edit produced from a different source version.
5. Cross-stylesheet conflicts retain their contributing rules without blocking independent work.
6. Default writes are all-or-nothing for the requested scope.
7. `--force` skips only recoverably failed package groups.
8. Automatic discovery respects Git ignore rules.
9. External workspace references prevent unsafe CSS Module deletion.
10. Preview, write, and repeated-run behavior are deterministic.

## Deferred Work

- SCSS, Sass, and Less parsers and source-preserving edits;
- Emotion and styled-components discovery from JS/TS inputs;
- JavaScript positional scope;
- configurable include/exclude globs;
- CSS-file-level failure isolation;
- dependency-graph ownership for shared CSS across multiple Tailwind applications;
- bounded parallel CSS parsing after benchmarks justify it.
