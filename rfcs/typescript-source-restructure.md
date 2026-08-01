# RFC: TypeScript Source Restructure

## Status

Proposed

## Summary

The published JavaScript layer currently lives as seven flat ESM files in the repository root (`bin/tw-migrate.js`, `bin/diagnostics.js`, `index.js`, `vue.js`, `html.js`, `style-compiler.js`, `native.js`) with a hand-written `index.d.ts`. This RFC moves those sources into `src/` as TypeScript, bundles them with tsdown through `vp pack` into `dist/`, and publishes `dist/` instead of the loose files.

The restructure is organizational. CLI behavior, the public `migrate()` API, planner requests, diagnostics, and every safety invariant remain byte-identical. The packaged CLI snapshot suite is the authority for proving that.

A `packages/*` monorepo split was considered and rejected. The repository publishes exactly one JavaScript package; the workspace exists only to carry the `npm/*` platform packages. A `packages/cli` package would force the napi output paths, the snapshot runner, the ecosystem harness, and CI to follow a second package boundary without adding a second package worth having. The monorepo split stays deferred until a second real package exists.

## Goals

1. Move all published JavaScript sources into `src/` as TypeScript.
2. Bundle with tsdown via `vp pack` into `dist/` with two entries: the CLI and the public API.
3. Generate `dist/index.d.ts` from source instead of maintaining a hand-written `index.d.ts`.
4. Preserve CLI output, exit statuses, public API shapes, and written bytes exactly.
5. Keep unit tests and local CLI runs buildless by executing `src/` directly through Node type stripping.
6. Keep the packaged snapshot suite validating the real installed `dist/` artifacts.
7. Split `index.ts` into modules along its existing responsibility seams once it is typed.

## Non-Goals

1. Creating `packages/*` workspace packages or moving the published package out of the repository root.
2. Changing CLI flags, output, diagnostics, warning codes, or exit behavior.
3. Changing the `migrate()` API surface or its documented shapes.
4. Changing the Rust planner, the NAPI contract, or the `npm/*` platform package layout.
5. Rewriting logic while converting files to TypeScript; conversion is one-to-one.
6. Adopting a `tsdown.config.ts`; Vite+ configuration stays in `vite.config.ts`.

## Source Layout

| Current                | New                         |
| ---------------------- | --------------------------- |
| `bin/tw-migrate.js`    | `src/bin.ts`                |
| `bin/diagnostics.js`   | `src/diagnostics.ts`        |
| `index.js`             | `src/index.ts`              |
| `vue.js`               | `src/vue.ts`                |
| `html.js`              | `src/html.ts`               |
| `style-compiler.js`    | `src/style-compiler.ts`     |
| `native.js`            | `src/native.ts`             |
| `index.d.ts` (curated) | generated `dist/index.d.ts` |
| `binding.d.ts` (root)  | unchanged (generated)       |

Build output:

```text
dist/bin.js       # CLI entry, shebang preserved
dist/index.js     # public API bundle
dist/index.d.ts   # generated declarations
```

`package.json` changes: `main` and `types` point into `dist/`, `bin` points to `dist/bin.js`, and `files` becomes `["dist"]`.

## Build and Packaging

- `vp pack` runs tsdown with a `pack` block in `vite.config.ts`: entries `src/bin.ts` and `src/index.ts`, ESM output, `dts` enabled.
- `parse5` is a runtime dependency and stays external; tsdown externalizes `dependencies` by default.
- The napi build keeps writing `tw-migrate.<target>.node` and `binding.d.ts` to the repository root. The relative addon lookup in `native.ts` becomes `../tw-migrate.<target>.node`, which resolves to the repository root both from `src/` during development and from `dist/` in the bundle; in the installed package it misses and resolution falls through to the platform packages. Both outputs remain generated and unpublished from the root package.
- In the installed package the local `.node` lookup fails as before and resolution falls through to the `tw-migrate-<target>` platform packages. That fallback chain is unchanged.
- The project-local Sass, Less, and Vue compiler loading uses `createRequire` resolution with runtime paths. tsdown cannot statically rewrite those loads, so the load-from-target-project invariant is structurally unaffected.

## Type Stripping Constraints

Published code must be built because Node refuses to type-strip files inside `node_modules`. Inside the repository, Node 22.18+ runs `src/*.ts` directly, which keeps development buildless:

- Local CLI runs use `node src/bin.ts`.
- `test/migrate.test.ts` imports `../src/index.ts` and `../src/style-compiler.ts`.

Sources therefore stay within erasable TypeScript: no enums, no namespaces, no parameter properties, and type-only imports marked `import type`. `tsconfig.json` already enforces type-stripping-safe options for the `.ts` harness and extends to `src/`.

## Harness and Documentation Updates

- `ecosystem-ci/lifecycle.ts` imports the installed package as `join(installed.root, "index.js")` in two places. Both switch to resolving the installed package's `main` from its `package.json` instead of hardcoding a file name.
- `test/ecosystem-harness.test.ts` stages fixture packages whose `files` mirror the real layout; those fixtures follow the `dist/` layout.
- `README.md`, `AGENTS.md`, and CI workflows replace `node bin/tw-migrate.js` with `node src/bin.ts` and document `vp pack` where release artifacts are built.
- `snapshots:prepare` and the release pipeline run `vp pack` before packing so the published tarball always carries a fresh `dist/`.

## Implementation Phases

Three stacked pull requests via gh-stack. Each phase changes one kind of diff so the snapshot and byte-exact suites attribute any breakage to exactly one cause.

### Phase 1: Restructure

Move the seven files into `src/` unchanged (still `.js` bodies under `.ts`-ready layout, moved as-is), wire `vp pack`, point `package.json` at `dist/`, relocate the napi outputs, and update the harnesses and docs. No source content changes beyond import paths.

Exit criteria: every Node test and packaged snapshot passes without a single snapshot update.

### Phase 2: TypeScript Conversion

Convert one file per commit, smallest first: `native`, `diagnostics`, `style-compiler`, `html`, `vue`, `bin`, `index`. Each commit renames one file to `.ts` and adds types only. The final commit deletes the curated `index.d.ts` in favor of the generated declarations after diffing the two for public-surface parity.

Exit criteria: `vp check` green, all suites pass, generated declarations expose the same public API as the curated file did.

### Phase 3: Module Split

Split `index.ts` along the responsibility seams `AGENTS.md` already names: package and workspace discovery, planner request preparation, diff rendering, and transactional writes. Pure code motion; the compiler proves the moves.

Exit criteria: `tsc --noEmit` green, all suites pass, `index.ts` retains orchestration only.

## Testing Strategy

- Phase 1 is validated by the absence of change: `vp run test` and `vp run test:snapshots` must pass with zero snapshot churn.
- Phase 2 relies on the byte-exact assertions in `test/migrate.test.ts` per converted file, plus a one-time declaration diff for the `index.d.ts` swap.
- Phase 3 relies on the type checker for reference integrity and the full suite for behavior.
- The packaged snapshot suite remains the only layer that executes `dist/`; unit tests intentionally execute `src/`.

## Success Criteria

1. `npm pack` output contains `dist/` and nothing from the old flat layout.
2. Packaged CLI snapshots pass unchanged across all three phases.
3. `vp check`, `vp run test`, and `vp run test:snapshots` pass at every stack level.
4. The generated `dist/index.d.ts` is public-surface-equivalent to the retired hand-written file.
5. Local development needs no JavaScript build step for tests or CLI runs.

## Accepted Trade-offs

1. Unit tests no longer execute the shipped bundle; a tsdown transform regression surfaces first in the packaged snapshot suite. That suite is already a required pre-commit gate for CLI-visible changes.
2. Generated declarations replace a curated `index.d.ts`; documentation comments must live in source JSDoc to survive generation.
3. Publishing and snapshot preparation gain a mandatory `vp pack` step.

## Deferred Work

1. A `packages/*` monorepo split, reconsidered only when a second publishable JavaScript package exists.
2. Module extraction beyond the four seams named in Phase 3.
