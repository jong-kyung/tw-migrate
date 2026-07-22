# RFC: Preprocessor and Static HTML Migration

## Status

Accepted

## Summary

`tw-migrate` currently discovers plain CSS and rewrites JavaScript/TypeScript JSX consumers. This RFC extends the migration pipeline to authored `.scss`, `.sass`, and `.less` stylesheets and static `.html` consumers while preserving the existing conservative safety model.

Preprocessor support means that the target project's own Sass or Less compiler evaluates the language's real semantics. It does not mean every generated declaration is automatically editable. Authored preprocessor source is changed only when generated CSS can be mapped back to one unambiguous original source range and the edit remains safe for every entry and consumer that shares it. Ambiguous constructs remain unchanged with warnings.

Static HTML support is limited to literal `class` and `id` attributes reached through local external stylesheet links. It does not introduce support for template languages, inline styles, or runtime DOM analysis.

## Goals

1. Accept `.css`, `.scss`, `.sass`, and `.less` authored stylesheet targets.
2. Evaluate SCSS/Sass/Less with the compiler installed in the target package.
3. Preserve the original stylesheet language and all unrelated source bytes.
4. Convert compiler-resolved values to exact Tailwind theme utilities or arbitrary values.
5. Support `.module.scss`, `.module.sass`, and `.module.less` in existing JS/TS JSX flows.
6. Analyze only preprocessor entries reachable from supported consumers; treat imported partials as dependencies rather than standalone entries.
7. Edit a shared partial only after every reachable entry and consumer proves the same edit safe.
8. Support literal `class` and `id` attributes in static `.html` files.
9. Scope HTML matching through local `<link rel="stylesheet">` and stylesheet import graphs.
10. Connect linked generated CSS to one unique same-stem preprocessor entry without requiring a prior build.
11. Remove a fully migrated CSS Module and each writable direct HTML link that consumed it.
12. Preserve the current package-batch snapshot, conflict, validation, and atomic-write guarantees.
13. Preserve package-level `--force` failure isolation.

## Non-Goals

1. Vue, Svelte, Astro, Angular, PHP, ERB, or other template languages.
2. HTML `<style>` blocks or `style` attributes.
3. Template expressions, bound attributes, or runtime-generated classes in `.html` files.
4. Automatic loading or execution of Vite, Webpack, PostCSS, or framework configuration.
5. Custom Sass importers, Sass plugins, Less plugins, or build-tool aliases.
6. Bundling Sass or Less with `tw-migrate`.
7. Replacing authored preprocessor files with generated CSS.
8. Editing generated CSS or generated source maps.
9. Automatic deletion of global stylesheet rules.
10. Guessing generated CSS-to-preprocessor relationships when no unique same-stem entry exists.
11. Automatic promotion of Sass/Less variables into Tailwind `@theme` variables.
12. A guarantee that every valid preprocessor construct is automatically migrated.

## Terminology

- **Authored stylesheet**: a user-owned `.css`, `.scss`, `.sass`, or `.less` source file.
- **Entry**: a stylesheet directly consumed by JS/TS, directly selected with `styleFile`, directly linked from HTML, or identified from linked generated CSS by a unique same-stem filename.
- **Partial**: a stylesheet loaded by an entry through CSS import, Sass `@use`/`@forward`/`@import`, or Less import semantics.
- **Compiled context**: one entry's generated CSS, source map, loaded-source graph, and accumulated consumer condition such as link media.
- **Provenance**: evidence connecting a generated CSS rule or declaration to one authored file and source span.
- **Safe edit**: a non-overlapping authored-source edit with unique provenance that remains valid in every consuming compiled context.

## Public Contract

### CLI

The positional argument becomes a stylesheet path rather than a CSS-only path:

```bash
# Preview all reachable stylesheets in the current package.
tw-migrate

# Preview one authored stylesheet.
tw-migrate src/components/Button.module.scss

# Migrate a package and its static HTML consumers.
tw-migrate --write

# Tailwind's entry remains plain CSS.
tw-migrate src/styles/site.less --tailwind-css src/tailwind.css --write
```

The usage text changes from `[css-file]` to `[style-file]`.

### Node.js API

`cssFile` is removed and replaced immediately by `styleFile`. No compatibility alias is provided.

```ts
interface MigrateOptions {
  styleFile?: string;
  cwd?: string;
  write?: boolean;
  tailwindCss?: string;
  workspaces?: boolean;
  force?: boolean;
}
```

`tailwindCss` continues to accept only `.css` files.

### Reports

The existing report shape remains. Preprocessor-specific state is represented through stable warnings rather than a new top-level field.

A changed authored preprocessor source produces one warning per affected entry:

```ts
{
  code: 'rebuild-required',
  file: 'src/styles/site.scss',
  start: 0,
  end: 0,
  message: 'Rebuild this preprocessor entry to refresh its generated CSS.'
}
```

Generated CSS and source maps are never listed as changed files unless they are independent authored CSS migration targets.

## Supported Inputs

### Stylesheets

| Extension | Parser syntax | Semantic evaluator |
| --- | --- | --- |
| `.css` | `Syntax::Css` | none |
| `.scss` | `Syntax::Scss` | target package's `sass` |
| `.sass` | `Syntax::Sass` | target package's `sass` |
| `.less` | `Syntax::Less` | target package's `less` |

CSS Modules are recognized by `.module.css`, `.module.scss`, `.module.sass`, and `.module.less`.

### Consumers

Existing JS/TS extensions remain supported. `.html` is added as a static consumer format.

HTML migration reads only:

- local `<link rel="stylesheet" href="…">` references;
- literal `class` attributes;
- literal `id` attributes;
- supported `media` conditions attached to stylesheet links.

Remote, protocol-relative, `data:`, and dynamically generated links are ignored with warnings when they block a migration.

## Architecture

```text
resolve package/workspace scope
  -> snapshot relevant authored styles, JS/TS, HTML, manifests
  -> discover consumer-to-entry relationships
  -> load project Sass/Less compiler when required
  -> compile each reachable preprocessor entry with source maps
  -> build entry/partial/link/import graphs
  -> parse compiled CSS for candidates
  -> prove generated-to-authored provenance
  -> plan JS/TS and HTML consumer edits together
  -> retain cross-stylesheet conflicts
  -> admit only all-consumer-safe authored edits
  -> validate candidates, syntax, recompilation, and snapshots
  -> preview or atomically write one transaction
```

### Responsibility Split

The Node layer owns:

- filesystem discovery and immutable snapshots;
- package ownership;
- project-local Sass/Less loading;
- compilation and compiler-loaded dependency discovery;
- source-map normalization;
- HTML link and stylesheet entry graph construction;
- Tailwind loading and candidate validation;
- final merged preview/write transaction.

The Rust layer owns:

- dialect-aware stylesheet AST parsing;
- selector and declaration planning;
- generated rule provenance consumption;
- JS/TS semantic rewriting;
- static HTML attribute rewriting;
- cross-stylesheet conflict handling;
- non-overlapping source edits and syntax validation.

A small private Node module should isolate compiler adapters and source-map normalization from `index.js`. Sass and Less are loaded with `createRequire()` rooted at the target package, following the existing Tailwind-loading pattern.

## Discovery and Graph Rules

### Entry Admission

Automatic migration does not treat every preprocessor file as an independent entry. Entries are admitted only when reached from:

1. a supported JS/TS stylesheet import;
2. a local HTML stylesheet link;
3. the unique non-partial preprocessor file whose stem matches a linked CSS filename;
4. another admitted stylesheet's import graph where that stylesheet is itself an executable entry; or
5. an explicit `styleFile` selection.

An explicitly selected partial is evaluated through every discovered entry that consumes it. If no complete consuming entry set can be established, it is retained rather than compiled as an invented entry.

### Compiler-Reported Dependencies

Sass and Less compiler-loaded URLs/files are authoritative for entry-to-partial edges. A conservative static import scan may suppress filename inference, but it never admits dependencies or replaces the compiler-loaded graph.

Only standard compiler resolution is enabled. Build-tool aliases, custom importers, and plugins are not loaded. An unresolved compiler dependency is a recoverable package input failure.

### HTML Relationships

A static HTML page is affected only by styles reachable through its local stylesheet links and transitive local CSS imports.

HTML migration does not inspect source maps emitted by prior project builds. Whether the linked CSS exists or not, it connects the link to a preprocessor entry only when exactly one non-partial file in the package has the same filename stem. The inferred relationship emits `inferred-preprocessor-source`; zero or multiple matches are not inferred. Once admitted, the project compiler's in-memory source map remains required to prove generated-to-authored edit spans.

Root-relative links are resolved against the package root. Query strings and fragments are excluded from filesystem resolution but preserved in HTML. Remote or virtual URLs are not writable graph edges.

When every rule and every JS/TS and static HTML reference to a CSS Module is migrated, each writable direct HTML `<link>` for that module is removed with the now-unused module dependency. A reference-only HTML file, dynamic attribute, unsupported media condition, retained rule, or otherwise unproven consumer retains both the module and its link. Transitive links are not removed; their stylesheet import edge retains the dependency instead.

### Shared Partials

One authored partial may occur in several compiled contexts. A proposed edit is admitted only when:

1. every reachable entry containing the source range was compiled;
2. every affected writable and reference-only consumer was analyzed;
3. every context proposes the same authored path, span, and replacement;
4. no context retains the rule or class dependency;
5. the edit introduces no cross-context Tailwind conflict; and
6. every affected entry recompiles after the edit.

Otherwise the source remains unchanged and the affected rule is reported as retained.

## Preprocessor Compilation

### Compiler Ownership

`tw-migrate` does not bundle Sass or Less. It resolves `sass` or `less` from the target package. The project therefore controls the compiler version used by its own migration.

If a required compiler cannot be resolved:

- normal mode fails the package operation before writing;
- workspace `--force` records a package failure and continues with admitted package groups.

### Analysis-Only Output

Compiler-generated CSS and maps are ephemeral planning inputs. They are never substituted for authored source.

Compiler results must include:

- generated CSS;
- a source map when authored edits depend on generated values;
- the entry path;
- normalized loaded source paths;
- enough origin information to identify writable project sources.

Virtual, external, dependency-owned, or unsnapshotted sources are analysis-only and cannot receive edits.

### Resolved Values

A generated declaration value may produce:

1. an exact utility from the target Tailwind theme;
2. a validated arbitrary value; or
3. an unsupported-declaration warning.

For example:

```scss
$space: 13px;
.card { padding: $space; }
```

may produce `p-[13px]`. The variable definition is not automatically removed, even if it becomes unused.

## Provenance and Source Editing

Generated CSS spans and authored source spans are distinct types in the planning contract. A generated span must never be applied directly to an authored preprocessor source.

A preprocessor declaration or rule is editable only when source-map and AST evidence resolves it to one contiguous authored span. The following are retained:

- missing or split mappings;
- mappings spanning multiple authored files;
- interpolation-generated selectors;
- declarations generated by ambiguous mixin/function expansion;
- Less inline JavaScript;
- generated or virtual sources;
- differing edits proposed by separate entries;
- edits whose post-edit compilation cannot be verified.

Edits are applied as byte-range splices to original source. The tool does not print or serialize a whole stylesheet AST.

After tentative authored edits, every affected entry is recompiled. The final generated CSS must parse, the expected removed or retained behavior must hold, and candidate validation must still succeed before the edit enters the transaction.

## Static HTML Rewriting

HTML uses a span-bearing parser. Regular expressions are not used to rewrite attributes.

A `class` or `id` attribute is writable only when:

- the parser provides an original byte location;
- its value is a plain literal;
- it contains no recognized template or binding syntax;
- the element is reached by a proven linked stylesheet context; and
- the planned candidates do not conflict with another linked stylesheet.

The implementation edits only the attribute value span and preserves quote style, tag formatting, comments, entities, and unrelated bytes.

The following remain unchanged:

```html
<div class="card {{ state }}"></div>
<div :class="state"></div>
<div class="card" style="padding: 1rem"></div> <!-- style is ignored -->
<style>.card { padding: 1rem }</style>            <!-- block is ignored -->
```

`<template>` descendants containing plain literal HTML are eligible. Script contents are never scanned for class strings.

### Link Media

A stylesheet link without `media`, or with `media="all"`, applies normally. Supported conditions are converted to the same Tailwind variants used for equivalent CSS `@media` conditions and are stacked outside rule-level variants.

If a link media condition cannot be represented exactly, that link context does not produce consumer edits and emits a warning. The condition is never ignored.

## Global Rules and CSS Modules

Global stylesheet rules are never deleted, regardless of whether their consumer is JSX or HTML and regardless of source language. Utilities may be appended to proven consumers, and the existing `retained-global-rule` warning remains.

Preprocessor CSS Module cleanup follows existing module safety rules plus provenance and shared-partial proof. A module reference or rule is removed only when every declaration, source origin, imported dependency, and consumer is safe. Otherwise both the source rule and required module reference remain.

## Validation and Transaction Policy

All existing batch invariants remain mandatory:

1. relevant inputs are snapshotted before planning;
2. all package edits target one immutable snapshot;
3. cross-stylesheet conflicts are resolved before source edits;
4. every Tailwind candidate compiles in the target package;
5. edited JS/TS, HTML, CSS, SCSS, Sass, and Less inputs reparse;
6. affected preprocessor entries recompile;
7. overlapping or disagreeing authored edits abort planning;
8. snapshots are checked immediately before writing;
9. all files are staged before any destination is replaced;
10. rollback restores every available backup after a failed write;
11. a successful second run produces no diff.

### Failure Classification

Recoverable package input failures include:

- missing project Sass/Less compiler;
- invalid initial preprocessor syntax;
- standard import resolution failure;
- invalid initial writable HTML input;
- unsupported source type.

Warnings retain individual constructs when a file remains analyzable, including ambiguous provenance, dynamic HTML attributes, unsupported link media, and shared partials without complete edit proof.

Candidate failures, generated edit parse failures, post-edit compiler failures, source-map/edit-integrity errors, changed snapshots, staging errors, and rollback failures remain fatal even with `--force`.

## Diagnostics

New stable warning categories include:

- `rebuild-required`;
- `unproven-source-map`;
- `shared-preprocessor-source`;
- `dynamic-html-attribute`;
- `unproven-script-reference`;
- `unsupported-html-base`;
- `unsupported-html-stylesheet-link`;
- `unsupported-link-media`;
- `cross-package-stylesheet-link`;
- `inferred-preprocessor-source`.

Warnings retain the existing `{code, file, start, end, message}` shape and deterministic ordering.

## Implementation Phases

### Phase 1: Generalized Stylesheet Contract

- Rename `cssFile` to `styleFile` in CLI, API, types, and errors.
- Add shared stylesheet extension/syntax/module helpers.
- Pass explicit syntax and module metadata into Rust.
- Parse and validate `.css`, `.scss`, `.sass`, and `.less` with Oxc.
- Keep Tailwind entry discovery and validation CSS-only.
- Add regression fixtures for discovery, explicit selection, modules, and failures.

Phase 1 does not claim evaluated variables or source-map-backed cleanup; constructs without direct safe values remain unchanged.

### Phase 2: SCSS and Sass Semantics

- Load target-package `sass`.
- Compile admitted entries with source maps and loaded source reporting.
- Build Sass entry/partial graphs.
- Resolve generated values and prove authored provenance.
- Recompile all affected entries after tentative edits.
- Emit deterministic rebuild warnings.

### Phase 3: Less Semantics

- Load target-package `less`.
- Add Less entry/import graph and source-map normalization.
- Apply the same provenance, shared-source, recompilation, and warning rules.

### Phase 4: Static HTML Consumers

- Add span-bearing static HTML parsing and attribute edits.
- Build local link/import and same-stem filename relationships.
- Add supported link-media variants.
- Route HTML matches through batch conflict handling.
- Preserve global rules and reject dynamic/template attributes.

Each phase is independently testable and may merge separately. Public documentation must describe only phases that are actually released.

## Testing Strategy

### Contract and Discovery

- `styleFile` accepts all supported extensions.
- legacy `cssFile` does not select a target.
- `styleFile` cannot be combined with `workspaces`.
- `tailwindCss` rejects non-CSS entries.
- automatic discovery admits entries but not standalone partials.
- ignored automatic files remain analysis-only; explicit targets preserve existing override behavior.

### Dialects and Modules

- plain CSS behavior remains byte-for-byte compatible.
- `.module.scss`, `.module.sass`, and `.module.less` resolve existing static JSX module forms.
- syntax selection is correct for all four extensions.
- malformed inputs follow default and `--force` package behavior.

### Semantic Evaluation

- Sass and Less variables resolve to exact or arbitrary utilities.
- nested selectors and supported conditions produce equivalent variants.
- mixin, function, interpolation, guard, and generated-selector cases migrate only with unique provenance.
- compiler absence and standard import failure are deterministic.
- project compiler resolution is package-relative.

### Graph and Provenance

- imported partials are not independently compiled as entries.
- extensionless, underscore, index, `@use`, `@forward`, and Less import graphs use compiler-reported dependencies.
- shared partials edit only after all entry contexts agree.
- missing, split, virtual, and conflicting compiler maps retain source.
- post-edit recompilation failure writes nothing.
- changed preprocessor sources emit one rebuild warning per affected entry.

### HTML

- local linked CSS updates literal class and id attributes.
- unlinked pages and unrelated styles remain unchanged.
- transitive local CSS imports are honored.
- linked generated CSS reaches a preprocessor source only through one unique same-stem filename, including before the first build and regardless of prior build maps.
- supported link media stacks variants; unsupported media retains.
- quote style and unrelated HTML bytes remain unchanged.
- template-looking values, bound attributes, inline styles, style blocks, scripts, and remote links are not rewritten.
- linked cross-stylesheet conflicts retain contributing rules and consumer edits.
- fully migrated CSS Modules remove direct writable HTML links, including HTML-only and inferred-preprocessor consumption.
- reference-only HTML, entity-bearing links, dynamic attributes, and unsupported link media cannot leave a dangling link.

### Transaction and Regression

- preview writes nothing.
- write uses one package transaction.
- workspace `--force` skips only recoverable package groups.
- snapshot, candidate, overlap, post-edit compile, and write failures abort.
- interrupted writes restore HTML and partial files.
- all existing CSS/JS/TS tests remain green.
- a successful second invocation produces zero diff.

## Success Criteria

1. The CLI and Node API accept the released stylesheet extensions through `styleFile`.
2. Tailwind entries remain `.css` and candidate validation uses the target package's Tailwind v4 installation.
3. Preprocessor values are evaluated by the target package's own compiler.
4. No generated CSS span is ever applied to authored preprocessor source.
5. Ambiguous mappings and incomplete consumer graphs retain source with warnings.
6. Shared partial edits are safe across every reachable entry and consumer.
7. Static HTML edits are link-scoped, literal-only, and byte-local.
8. Global rules are never automatically deleted.
9. Generated CSS is not edited; changed preprocessor entries produce rebuild warnings.
10. A CSS Module is unlinked from static HTML only when every rule and consumer is safely migrated.
11. Existing snapshot, conflict, atomic write, rollback, `--force`, determinism, and idempotence guarantees remain intact.

## Accepted Trade-offs

1. Full language evaluation produces conservative conversion coverage; valid constructs may remain when source provenance is ambiguous.
2. Projects must already install the compiler required by their authored language.
3. Custom build resolution is unsupported unless the standard compiler can resolve the graph independently.
4. HTML-to-preprocessor relationships ignore prior build maps, use a warned same-stem filename inference, and skip ambiguous matches.
5. Generated CSS may remain absent or stale until the project rebuilds it.
6. Global migrations intentionally duplicate validated utilities while retaining authored global rules.
7. The immediate `cssFile` to `styleFile` rename is a breaking API change.

## Deferred Work

- Explicit custom load paths or alias mapping.
- Optional build-script execution after write.
- Additional HTML-like template languages.
- Inline style migration.
- Automatic `@theme` extraction from preprocessor variables.
- Generated artifact refresh.
- Configurable global-rule cleanup after runtime evidence.
