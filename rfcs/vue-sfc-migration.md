# RFC: Vue Single-File Component Migration

## Status

Accepted (Phase 1 implemented)

## Summary

`tw-migrate` currently migrates plain CSS, SCSS, Sass, and Less stylesheets
consumed by JS/TS JSX and static HTML. This RFC extends the pipeline to Vue 3
single-file components (`.vue`), where one file is both a stylesheet source
(`<style>` blocks) and a consumer (`<template>` class usage).

Support ships in staged phases. Phase 1 is closed over a single file: literal
`class` attributes in `<template>` and plain-CSS `<style scoped>` blocks in the
same SFC. Later phases add external stylesheet imports, preprocessor style
blocks, literal `:class` bindings, and `<style module>`/`$style`.

SFC parsing uses the official `@vue/compiler-sfc`, resolved from the target
project like Sass and Less. The unofficial Rust toolchain `vize` was evaluated
and rejected for now: it is pre-1.0, unofficial, and this tool writes bytes to
user source, so parser maturity outranks single-language integration. Revisit
if vize reaches a stable release or official adoption.

## Goals

1. Accept `.vue` files as discovered inputs and as explicit `styleFile`
   targets in Vue 3 packages.
2. Parse SFCs with the target project's own `@vue/compiler-sfc`.
3. Rewrite literal `<template>` `class` attributes to Tailwind utilities using
   rules proven from the same file's plain-CSS `<style scoped>` blocks.
4. Remove a scoped rule only when every element it can match is proven inside
   the file, including injection surfaces (component tags, root fallthrough,
   dynamic bindings).
5. Retain every unproven construct with a stable warning, mirroring the
   existing conservative model.
6. Remove a fully migrated `<style scoped>` block, mirroring fully migrated
   stylesheet deletion.
7. Preserve byte offsets, quote style, and all untouched bytes in `.vue`
   files.
8. Preserve the existing snapshot, conflict, atomic-write, `--force`,
   determinism, and idempotence guarantees.
9. Define the full roadmap (Phases 1–4) so later phases extend rather than
   rework the Phase 1 file model.

## Non-Goals

1. Vue 2 single-file components; they are retained whole with a warning.
2. Unofficial SFC compilers (`vize`, `fervid`) or bundling any Vue compiler
   with `tw-migrate`.
3. Non-default template languages (`<template lang="pug">`) and analysis of
   script contents or languages.
4. Converting `:deep()`, `:global()`, `>>>`, `/deep/`, or CSS `v-bind()`
   expressions into utilities.
5. Migrating `<style>` blocks without `scoped` in Phase 1; they are global CSS
   and require cross-file proof.
6. Runtime component-tree analysis or evaluation of dynamic `:class`
   expressions beyond the literal forms scheduled for Phase 3.
7. In-DOM templates, render functions, JSX-in-Vue, or `template` options in
   plain JS files.
8. Editing compiled SFC output; all edits target authored `.vue` bytes.

## Terminology

- **SFC**: a Vue 3 single-file component (`.vue`).
- **Block**: a top-level SFC section (`<template>`, `<script>`, `<style>`)
  with a known byte span in the file.
- **Scoped block**: a `<style scoped>` block; its rules apply to elements in
  this SFC's template and to the root elements of child components it renders.
- **Host element**: a plain HTML element in the template (`div`, `span`).
- **Component tag**: a template element that resolves to a component
  (capitalized, known custom element, or `<component>`).
- **Class site**: the byte span of one literal `class` attribute value in the
  template, expressed in absolute file offsets.
- **Root fallthrough**: Vue's attribute inheritance, which merges a parent's
  `class` onto a single-root component's root element.
- **Closed rule**: a scoped rule whose complete match set is proven from this
  file alone.

## Compiler Selection

`tw-migrate` does not bundle a Vue compiler. It resolves `vue/compiler-sfc`
from the target package with `createRequire()` rooted at that package,
following the Sass/Less and Tailwind loading pattern. The project's own
compiler version therefore defines what parses, exactly as it does in the
project's build.

Rationale, recorded for future re-evaluation:

- Migration needs parsing with faithful source offsets, not code generation.
  `@vue/compiler-sfc`'s `parse()` returns block descriptors and a template AST
  with source locations against the original file.
- Template syntax is versioned by the project's `vue` dependency; loading from
  the project removes parser-drift risk entirely.
- `vize` is unofficial and pre-1.0. A build tool mis-parsing fails a build; a
  migration tool mis-parsing writes wrong bytes to user source. The risk
  profiles are not comparable.

If `vue/compiler-sfc` cannot be resolved in a package containing discovered
`.vue` inputs, the package fails as a recoverable input failure: normal mode
fails before writing, and workspace `--force` records the failure and
continues with other packages, mirroring a missing Sass compiler. A resolved
Vue major version other than 3 retains the package's SFCs with
`unsupported-vue-version`.

## Public Contract

### CLI

`.vue` joins the supported `styleFile` extensions:

```bash
# Preview all reachable inputs, including SFCs, in the current package.
tw-migrate

# Preview one SFC.
tw-migrate src/components/Button.vue

# Migrate a package including SFCs.
tw-migrate --write
```

### Node.js API

`MigrateOptions` is unchanged. `styleFile` accepts `.vue` paths. Reports keep
the existing shape; SFC-specific state is expressed through warnings and the
existing changed/retained file lists.

### File Model

An SFC is simultaneously a stylesheet source and a consumer. In the planner
contract it contributes:

- one stylesheet input per plain-CSS scoped block, carrying the block's
  absolute byte range;
- one consumer input carrying the template's class sites in absolute file
  offsets.

All planned edits for one SFC land in one file entry and must be
non-overlapping, exactly like existing multi-edit files.

## Architecture

The Node layer follows the `html.js` pattern with a new `vue.js` module:

- resolve `vue/compiler-sfc` from the target package;
- parse each discovered SFC once per snapshot;
- reject the file (retain with a warning) on parse errors, Vue 2 syntax, or
  unsupported block kinds it cannot classify;
- extract style blocks with absolute content offsets and their attribute set
  (`scoped`, `module`, `lang`, `src`);
- lower the template AST to a compact contract: class sites, element kind
  (host or component tag), root/multi-root shape, and presence of dynamic
  class bindings;
- hand the contract to the Rust planner and apply returned byte edits through
  the existing transactional writer.

The Rust layer owns:

- parsing scoped block CSS with the existing CSS pipeline;
- selector-to-class-site matching, reusing the HTML matching model (literal
  `class` and `id` selectors; other selector forms follow existing CSS
  planning rules or retain);
- closure proofs and per-rule retention decisions;
- utility generation, conflict handling, and edit planning in absolute file
  offsets.

No whole-file AST is serialized across the NAPI boundary; the contract is a
small offset-annotated structure, like the HTML contract.

## Scoped Rule Semantics

A scoped rule is **rewritten** (utilities appended to matched class sites and
the rule removed) only when it is a closed rule. Each open surface downgrades
the rule to retain-with-append or retain-only:

1. **Component tags.** A scoped rule also matches a child component's root
   element, and that root's own classes come from the child file, which
   same-file analysis cannot see. Any component tag in the template therefore
   opens every scoped class rule: utilities are still appended to proven
   host-element sites, and the rules are retained with
   `component-class-target`. Phase 2's cross-file analysis can narrow this by
   reading the child components.
2. **Root fallthrough.** A parent can merge arbitrary classes onto a
   single-root component's root element, so any class rule could match the
   root. Class rules in a single-root SFC are therefore not closed; Phase 1
   recognizes exactly one closure proof: a multi-root template (two or more
   unconditionally rendered root nodes), which disables automatic attribute
   inheritance. Statically visible `inheritAttrs: false` is deferred to a
   later phase. Open rules append utilities to proven sites and retain with
   `open-root-fallthrough`. Phase 2's cross-file analysis can close this
   surface by proving no caller passes a `class` to the component.
3. **Dynamic class bindings.** `:class`, spread `v-bind`, and dynamic
   directive arguments (`v-bind:[key]`) make an element's class set opaque;
   an opaque set can contain any class. If the template contains any such
   binding — or a class/id attribute that is not a safely writable quoted
   literal — all scoped class rules retain with `dynamic-template-class` in
   Phase 1. Literal class attributes beside a dynamic binding remain proven
   sites and still receive appended utilities. Runtime class mutation through
   scripts, event handlers, refs, or custom directives is outside the supported
   scope, matching the React/JSX path. Phase 3 narrows dynamic bindings to
   literal object/array forms.
4. **Escape hatches.** Rules containing `:deep()`, `:global()`, `>>>`,
   `/deep/`, or declarations using `v-bind()` have no utility representation
   and retain through the existing selector/declaration support codes
   (`unsupported-selector`, `unsupported-value`, and friends).
5. **Cascade shadowing.** A scoped selector compiles with a `[data-v-*]`
   attribute and sits outside CSS layers, so it can outrank non-scoped CSS
   that the layered Tailwind utility replacing it would lose to. Before a
   closed file deletes a rule, the package's non-scoped corpus — other
   non-module stylesheets, retained SFC style blocks, scan-only (gitignored)
   SFC blocks, module CSS containing `:global` escapes, and the inner
   selectors of scope-escape pseudo-classes (`:deep()`, `:global()`,
   `:slotted()`, and their `::v-` aliases) from analyzable scoped blocks —
   is parsed as CSS and
   reduced to a selector index keyed by each selector's rightmost compound:
   its classes, ids, and element types. A rule is retained with
   `shadowed-scoped-rule` when the index targets one of its classes or can
   reach one of its template sites through the site's tag, id, or
   co-occurring classes. Module CSS joins through a separate channel that
   indexes only its global surface (type and attribute selectors), since
   module class and id names are localized at build time; `:global(...)`
   arguments are re-indexed as plain global selectors rather than
   invalidating the corpus. Retained rules in the same scoped block are
   competitors too: a rule sharing a site with any retained sibling is
   itself retained, iterated to a fixpoint — except retained at-rules
   (`@keyframes`), which select no elements. `<style src>` blocks may point
   outside the discovered corpus and mark it unverifiable. Anything
   the index cannot prove — pieces that fail to parse, universal or
   base-less selectors, preprocessor interpolation or `&`-concatenation,
   HTML sources containing inline `<style` blocks, unanalyzable SFCs, and
   unextractable escape forms (`>>>`, `/deep/`, combinator `::v-deep`,
   nested escape arguments) — marks the corpus unverifiable and retains
   every closed rule. Candidates that cannot be
   written inside an attribute's own quote delimiter are withheld and their
   rules retained, sharing the static-HTML quote handling.
6. **Directives that alter structure.** `v-for`, `v-if`/`v-else` duplicates
   or removes elements but does not change their literal class sets; matched
   sites under these directives are still proven hosts. `<slot>` content is
   rendered in the parent's scope and is not a match surface for this file's
   scoped rules, except that forwarded slot wrapper elements in this template
   remain ordinary hosts.

Retain-with-append mirrors the existing global-rule policy: validated
utilities are duplicated onto proven consumers while the authored rule stays,
and a second run produces no diff.

### Blocks

- A plain-CSS `<style scoped>` block whose rules are all removed is deleted
  whole, including its tags, mirroring fully migrated stylesheet deletion.
  A partially migrated block keeps its remaining rules byte-exactly, and
  conditionals that were already empty in the authored source (often
  comment-only) are never removed.
- `<style>` without `scoped` retains whole with `unscoped-style-block`.
- `<style lang="…">` retains whole with `preprocessor-style-block` until
  Phase 2.
- `<style module>` retains whole with `unsupported-sfc-block` until Phase 4.
- `<style src="…">` retains with `unsupported-sfc-block`; Phase 2 revisits it
  as an import edge.
- `<script>` contents and languages do not participate in template closure.
  Inline text is carried only by the existing CSS Module deletion guard;
  runtime class mutation remains outside the supported scope.

## Diagnostics

New stable warning codes, keeping the `{code, file, start, end, message}`
shape and deterministic ordering:

- `unsupported-vue-version`
- `unsupported-sfc-block`
- `unscoped-style-block`
- `preprocessor-style-block`
- `component-class-target`
- `open-root-fallthrough`
- `dynamic-template-class`
- `shadowed-scoped-rule`

A missing `vue/compiler-sfc` is a recoverable package failure ("Vue 3 with
compiler-sfc must be installed in the target project."), not a warning code,
mirroring a missing Sass compiler.

## Implementation Phases

### Phase 1: Same-File Static Migration (this RFC's committed scope)

- Add `.vue` discovery, explicit selection, and the dual-identity file model.
- Add `vue.js` with project-local `@vue/compiler-sfc` loading and the
  offset-lowering contract.
- Plan plain-CSS scoped blocks against literal template class sites in Rust
  with the closure rules above.
- Add packaged snapshot fixtures and one controlled ecosystem-ci Vue case.

### Phase 2: External Stylesheets and Preprocessor Blocks

- Treat `<script>` stylesheet imports (`.css`, `.module.css`, preprocessor
  variants) as consumer edges into the existing entry graph.
- Route `<style lang="scss|sass|less" scoped>` through the project compiler
  with block-relative source-map normalization on top of block offsets.
- Add cross-file caller analysis to close the root-fallthrough surface and to
  admit unscoped block migration where project-wide usage is proven.

### Phase 3: Literal `:class` Bindings

- Prove object literal (`:class="{ btn: cond }"`) and array literal forms,
  parsing embedded expressions with the existing oxc path at corrected
  offsets.
- Narrow the dynamic-binding retention from template-wide to element-wise for
  proven literal forms.

### Phase 4: CSS Modules (`<style module>` / `$style`)

- Prove `$style.x` usage across template and script, mirroring the existing
  JS CSS Modules flow.

Each phase merges separately. Public documentation describes only released
phases.

## Testing Strategy

### Node

- SFC block extraction returns absolute offsets that reproduce the original
  bytes for every block.
- Compiler resolution is package-relative; a repository-installed Vue must
  never satisfy a target project.
- Parse errors, Vue 2 sources, and unsupported blocks retain the file.

### Rust

- Closure proofs: component tags, root fallthrough, multi-root templates,
  dynamic bindings, and escape hatches each force the documented retention.
- Matched host sites receive utilities; removed rules empty their block;
  partial blocks keep remaining rules byte-exactly.
- Edits never overlap and never touch bytes outside planned spans.

### Packaged Snapshots

- A Vue fixture package covering: full block removal, retain-with-append,
  each warning code, missing `vue/compiler-sfc` under default and `--force`
  modes, and a Vue 2 package.
- Second-run idempotence over a migrated fixture.

### Ecosystem CI

- One controlled Vite + Vue 3 project in `projects.json` containing plain-CSS
  scoped blocks, static classes, and samples of every retention hole. The
  computed-style oracle must pass before Phase 1 ships; scoped semantics
  regressions are not observable from file diffs alone.

## Success Criteria

1. `.vue` files are discovered and selectable in Vue 3 packages, parsed by
   the target project's `@vue/compiler-sfc`.
2. Literal template classes gain utilities only from same-file scoped proof.
3. No scoped rule is removed while any injection surface remains open.
4. Every unproven construct retains with one of the stable warning codes.
5. Untouched `.vue` bytes are preserved exactly; edited files reparse with
   the project compiler.
6. Missing-compiler and Vue 2 packages follow the recoverable-failure and
   `--force` model.
7. The controlled ecosystem-ci Vue case passes the computed-style oracle.
8. Existing suites remain green and a second run produces no diff.

## Accepted Trade-offs

1. Root fallthrough and component tags keep most SFCs in retain-with-append
   mode until Phase 2's cross-file analysis; Phase 1 favors provable safety
   over removal coverage, and only multi-root component-free templates reach
   scoped-rule removal.
2. Any dynamic class binding retains all scoped class rules in Phase 1.
3. Real-world SFCs using `lang="scss"` see no Phase 1 changes.
4. Retain-with-append intentionally duplicates validated utilities alongside
   retained rules, as the global-rule policy already does.
5. Choosing the official JS compiler keeps template analysis contracts
   crossing the NAPI boundary instead of a single-language Rust pipeline.
6. A generated utility that overlaps a Tailwind class already present on
   the element is appended with an `existing-tailwind-conflict` warning and
   Tailwind's output order decides between them, matching the JS rewrite
   path.
7. Repeated same-selector rules with conflicting declarations inside one
   stylesheet migrate as they always have across the tool: both rules'
   utilities are appended and Tailwind's output order decides the winner,
   because relative precedence between distinct sources is not modeled. This
   long-standing engine-wide behavior is accepted for Vue as well.
8. The cascade-shadowing index is package-scoped and rightmost-compound
   based: a corpus selector retains the rule even when its ancestor parts
   could never be satisfied, and CSS outside the package is not seen.
9. Runtime class mutation through scripts, event handlers, refs, or custom
   directives is outside the supported scope and does not block migration of
   proven static template sites, matching the React/JSX path.

## Deferred Work

- ID-selector removal accounting: a proven `#id` scoped rule currently
  appends its utilities but is never removed, because module reference
  counting tracks classes only.
- Vue 2 support, pending demonstrated demand.
- `vize` re-evaluation at a stable or officially adopted release.
- Non-default template block languages.
- `@theme` extraction from `v-bind()` usage.
- Scoped `@keyframes` and animation name migration inside SFCs.
