# Concepts

Shared domain vocabulary for this project — entities, named processes, and status concepts with project-specific meaning. Seeded with core domain vocabulary, then accretes as ce-compound and ce-compound-refresh process learnings; direct edits are fine. Glossary only, not a spec or catch-all.

## Stylesheet migration

### Consumer
A file whose references to a stylesheet gate that stylesheet's migration: a JS/TS source importing it, an HTML page linking it or using its classes, or another stylesheet composing or importing it. A stylesheet may be deleted only when every consumer's references are provably migrated; a consumer that cannot be fully analyzed blocks deletion instead.

### Dependent
A stylesheet that references another stylesheet through composition or an import. Dependent edges are recorded for every stylesheet the run can reach — including ones discovered late through HTML links — and any dependent edge onto a CSS Module blocks that module's deletion.

### Reference-only retention
The uniform fallback for any reference that cannot be proven safe to migrate: keep the referenced stylesheet unchanged, emit a named warning, and continue the run rather than aborting. Applies to unanalyzable consumers of every kind — re-exports, dynamic imports, unparseable or ignored files, foreign-package pages, inline scripts naming module classes.

Retention blocks deleting the stylesheet and removing links or imports to it, but does not block adding utilities to consumers that were safely analyzed.

### Candidate
A Tailwind utility string proposed to replace a stylesheet rule's declarations. A rule whose declarations cannot be expressed as candidates is retained during planning; when the project's own Tailwind installation cannot generate CSS for a candidate, its owning rule is blocked and the plan is recomputed until every applied candidate compiles.

### Tailwind entry
The CSS file that imports Tailwind itself. A run requires exactly one detected entry per package unless one is explicitly configured; entries are never migration targets, and only plain CSS may serve as one.

### Same-stem inference
Matching an HTML-linked generated CSS file to the unique preprocessor source sharing its filename stem, so analysis and edits target the authored source instead of the build artifact. Inference is skipped whenever another source imports the generated file directly, because migrating the authored source could orphan that import.
