# RFC: Delegate Source Analysis to Syntax Parsers

## Status

Proposed

## Summary

`tw-migrate` already uses Oxc to parse and analyze JavaScript and TypeScript in the Rust planner, `oxc-css-parser` to plan stylesheet rewrites, Vue's project-local compiler to parse SFCs, and parse5 to parse HTML. The TypeScript orchestration layer still performs several syntax decisions with regular expressions, substring searches, and character scanners.

This RFC moves syntax recognition to those existing parsers. One Oxc source-analysis API will report module records, component bindings, static literals, dynamic imports, and Vue-specific semantic references. One `oxc-css-parser` collector will report dependencies for CSS, SCSS, indented Sass, and Less. Vue and HTML analysis will consume structural compiler output wherever the upstream APIs expose it.

The Rust byte-span edit engine remains in place. `magic-string` will not be added because the migration does not produce JavaScript source maps, and moving edits into JavaScript would introduce UTF-8 byte to UTF-16 index conversion across the NAPI boundary without improving syntax analysis.

## Goals

1. Remove manual syntax recognition when Oxc, `oxc-css-parser`, the Vue compiler, or parse5 exposes the same structure.
2. Parse each JavaScript or TypeScript source once per orchestration analysis and return the facts needed by TypeScript callers through one NAPI API.
3. Collect stylesheet dependencies from structured AST nodes for CSS, SCSS, Sass, and Less.
4. Remove false positives caused by comments, string contents, shadowed identifiers, and similarly shaped syntax.
5. Preserve byte-exact edits and conservative retention when analysis cannot prove a rewrite safe.
6. Keep the existing writable versus scan-only parse-failure policy.

## Non-Goals

1. Generating JavaScript source maps.
2. Replacing the Rust edit planner or changing the public `migrate()` API.
3. Moving the package graph, Vue component graph, or migration orchestration into Rust.
4. Replacing target-project Sass, Less, or Vue compilation with parser output. Parsing discovers structure, but project-local compilers still define authored preprocessing behavior.
5. Removing source inspection needed to translate parser spans into byte-exact edit positions when an upstream parser does not expose the inner span.
6. Broadening the supported JavaScript, stylesheet, HTML, or Vue syntax beyond what the owning parser and current migration safety policy can prove.

## Current Problems

### Repeated JavaScript and TypeScript parsing

The native layer exposes separate helpers for static imports, default import bindings, direct static string expressions, and shared-entry module records. Callers invoke more than one helper for the same Vue script, so Oxc reparses the source. Other checks still inspect raw text for dynamic `import()`, Vue macros, `$style`, `useCssModule`, and string literals that might name Vue files.

These checks create false positives. A comment containing `import(` opens the Vue caller graph. A string containing `defineProps` marks fallthrough analysis unverifiable. A shadowed local named `useCssModule` looks like the Vue API. A random string ending in `.vue` can expose components even when JavaScript never uses it as a load target.

### Stylesheet dependency scanning

`indexStylesheetDependents()` masks comments and uses regular expressions for CSS Modules `composes`, Sass `@use` and `@forward`, and import forms. `cssImports()` scans quotes, braces, parentheses, and semicolons by hand.

`oxc-css-parser` 0.0.11 parses all four supported syntaxes and exposes structured `Import`, `LessImport`, `SassImport`, `SassUse`, and `SassForward` preludes. Declaration nodes also expose `composes` values. The regular-expression and character-scanner implementations duplicate grammar already available in the dependency.

### Vue and HTML source inspection

The project-local Vue compiler provides SFC descriptors and template locations, but the current analysis locates some block boundaries and scoped-selector escapes from raw text. parse5 provides element and whole-attribute locations but does not expose every inner value boundary needed by the byte-exact edit contract.

The implementation should consume structural locations first. It may retain a small source-position helper where the parser omits an inner span. Such helpers locate bytes inside parser-proven syntax, so they must not decide whether arbitrary source text forms valid syntax.

## Design

### Unified Oxc source analysis

Add one native `sourceAnalysis(path, source)` API. It parses the source with the same `SourceType` rules as the planner, builds semantic information once, and returns a structured result.

The initial result contains:

- static and dynamic module records, including type-only and import-attribute classification;
- default import bindings used by the Vue component graph;
- static string literals in syntax positions used by conservative Vue exposure analysis;
- whether a real dynamic `import()` expression occurs;
- references to unbound Vue compiler macros that affect fallthrough analysis;
- real `useCssModule()` calls and `$style` references used by CSS Module closure analysis.

Collectors must use AST node kinds and semantic symbol resolution. Comments and unrelated string contents never count. A local binding shadows a compiler macro or Vue helper unless the code imports the real helper in a form the collector recognizes.

TypeScript memoizes analysis by path and source, matching the current shared-entry import cache. Existing helper exports may remain as short compatibility wrappers while internal callers migrate, but they must not remain independent full-program parsers.

### Stylesheet dependency collector

Add one native collector that accepts the stylesheet path or syntax plus source and returns structured dependency references.

It parses with:

| Extension | Oxc syntax     |
| --------- | -------------- |
| `.css`    | `Syntax::Css`  |
| `.scss`   | `Syntax::Scss` |
| `.sass`   | `Syntax::Sass` |
| `.less`   | `Syntax::Less` |

The collector walks parser-proven dependency forms:

- CSS `@import`;
- Sass `@import`, `@use`, and `@forward`;
- Less imports;
- CSS Modules `composes: ... from "..."` declarations.

It returns literal references only. Interpolated, computed, or otherwise unreadable references make the dependency surface unverifiable. The TypeScript graph records that state instead of guessing from source text.

`collectCssDirectives()` remains responsible for Tailwind entry and `@source` metadata. Its internal handling should use structured Oxc component values where available. Tailwind-specific prelude grammar may use a bounded parser over parser-proven prelude tokens because Oxc does not assign Tailwind semantics to custom directives.

### Vue analysis

Vue SFC structure continues to come from the target project's Vue 3 compiler. JavaScript and TypeScript facts for each inline script block come from `sourceAnalysis()`.

The Vue component graph uses AST-classified imports, dynamic imports, and relevant static literal syntax. It no longer searches all quoted source text. Fallthrough and CSS Module closure checks use semantic flags rather than identifier-name regular expressions.

Scoped style escape analysis should use `oxc-css-parser` selector nodes where the parser represents the syntax. Unsupported legacy or malformed escape forms make the scoped shadow surface unverifiable and retain affected rules. The implementation must not recover nested selector syntax with a parenthesis regular expression.

### HTML analysis

parse5 remains the HTML parser. Element, tag, and attribute existence come only from its tree and location records. Template-marker detection remains a safety classification for attribute values because parse5 treats framework template text as ordinary HTML text.

Where parse5 reports only a whole-attribute span, a bounded helper may inspect that parser-proven slice to find the exact value bytes and quote style. The helper must reject ambiguous or entity-bearing values. It cannot discover attributes or tags by scanning the document.

### Edit engine

Rust continues to own edits as UTF-8 byte spans with `{ start, end, replacement }`. `apply_edits()` sorts edits, rejects overlaps and invalid spans, and copies untouched bytes once.

`magic-string` uses JavaScript string indices and would require offset translation for every Rust span. Its source-map support has no consumer in the current API. The existing edit engine is smaller and enforces the repository's byte-preservation invariant at the layer that produces edits.

## Failure Policy

The migration keeps its existing distinction:

- A writable JavaScript or TypeScript migration target that Oxc cannot parse or analyze fails planning.
- A scan-only source that cannot be parsed remains opaque. If it may reference a stylesheet or component surface, the planner retains the affected rules and emits the existing conservative warning.
- A stylesheet dependency source that `oxc-css-parser` cannot parse leaves its dependency surface unverifiable. The orchestrator retains affected rules rather than falling back to regular expressions.
- Unsupported Vue or HTML structure follows the existing retention and warning policy.

`--force` may skip recoverable package input failures. It must not hide integrity failures, edit collisions, or invalid edited output.

## Observable Behavior Changes

The parser-backed analysis may migrate rules that the text checks retained incorrectly. Expected examples include:

- `import(` inside a comment or string;
- `defineProps`, `defineOptions`, or `inheritAttrs` used as string content;
- a shadowed local identifier with a Vue macro name;
- `$style` or `useCssModule` inside comments and strings;
- `.vue` text that does not participate in a relevant JavaScript load expression;
- commented-out stylesheet dependency directives.

These changes are accepted when AST and semantic analysis prove the old result was a false positive. Actual dynamic, aliased, interpolated, or unparseable references remain conservative.

## Implementation Phases

### Phase 1: JavaScript and TypeScript source analysis

1. Add the native source-analysis result and one Oxc parse and semantic pass.
2. Cover runtime and type-only imports, dynamic imports, default bindings, relevant static literals, and Vue semantic flags in Rust tests.
3. Add a TypeScript cache keyed by path and source.
4. Migrate shared-entry and Vue callers from separate native helpers and raw-text checks.
5. Add Node tests for comments, strings, shadowed identifiers, parse failures, and real dynamic references.

Exit criteria: JavaScript and TypeScript syntax decisions in the orchestration layer come from `sourceAnalysis()`, focused Rust and Node tests pass, and writable versus scan-only failures retain current behavior.

### Phase 2: Stylesheet dependency analysis

1. Add the four-syntax native dependency collector.
2. Replace `cssImports()` and `indexStylesheetDependents()` grammar scans.
3. Return and propagate an unverifiable dependency state.
4. Remove regex fallback from Tailwind import discovery when parser failure cannot prove an entry.
5. Add Rust fixtures for CSS, SCSS, Sass, Less, interpolation, comments, and malformed input.

Exit criteria: no production regular expression or character scanner discovers stylesheet imports, Sass module dependencies, or CSS Module composition dependencies.

### Phase 3: Vue, HTML, and residual audit

1. Use Vue descriptors and Oxc source analysis for SFC script structure.
2. Replace scoped escape text extraction with selector AST analysis or conservative retention.
3. Limit HTML source inspection to parser-proven attribute slices that need inner byte spans.
4. Audit remaining production regexes and scanners. Classify each remaining use as path handling, value validation, parser-proven span location, or unsupported domain grammar.
5. Remove any residual syntax recognition that an existing parser can provide.

Exit criteria: every remaining source-text inspection has a documented byte-location or domain-validation purpose and does not duplicate a parser-supported grammar.

## Testing Strategy

Each phase adds the smallest test that fails under the text-based implementation and passes with parser-backed analysis.

- Rust unit tests own Oxc AST and semantic collector behavior.
- `test/migrate.test.ts`, `test/shared-entry.test.ts`, and Vue-focused tests own orchestration and retention behavior.
- Packaged CLI snapshots own warning text, converted and retained counts, exit status, and written bytes.
- Existing byte-exact and source-integrity tests must pass without weakening assertions.

Run focused tests while implementing each phase. Before completion run:

```bash
vp check
vp run test
vp run test:snapshots
```

Snapshot updates require a parser-proven false-positive removal or a documented warning-policy change. Broad normalization changes are out of scope.

## Success Criteria

1. TypeScript orchestration does not parse JavaScript or TypeScript syntax with regular expressions or substring searches.
2. One cached native source analysis supplies the orchestration facts for each JS, TS, JSX, TSX, and Vue script source.
3. `oxc-css-parser` supplies dependency records for CSS, SCSS, Sass, and Less.
4. Comments, strings, and shadowed identifiers no longer create syntax false positives.
5. Parse failures retain or fail according to the existing writable versus scan-only policy.
6. Byte-exact edit tests and source-integrity checks remain unchanged and pass.
7. No `magic-string` runtime dependency is added.

## Accepted Trade-offs

1. Parser failure can retain more CSS than a text fallback would. The fallback cannot prove syntax and conflicts with the migration's safety model.
2. The NAPI source-analysis result contains facts for several TypeScript consumers. This creates a wider internal contract but avoids repeated parsing and keeps syntax ownership in one layer.
3. Parser upgrades may change which formerly ambiguous inputs become provable. Tests must describe those boundaries instead of pinning known false positives.

## Deferred Work

1. Source-map output for rewritten JavaScript. Add it only with a public consumer and an API contract.
2. Upstream contributions that expose more precise parse5 or Vue compiler spans. Adopt them when they remove a bounded source-position helper without reducing supported inputs.
3. Moving higher-level package or component graph construction into Rust. Reconsider only if profiling shows NAPI result transfer or TypeScript graph traversal is material.
