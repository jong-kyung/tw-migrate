# AI Agent Guidelines for tw-migrate

`tw-migrate` converts static React/Next.js, Vue 3 SFC, and HTML stylesheet usage to Tailwind CSS v4 utilities. `CLAUDE.md` points here.

Read [CONTRIBUTING.md](CONTRIBUTING.md) for setup, validation commands, packaging, and CI. Use the implementation and [RFCs](rfcs/) for detailed behavior rather than duplicating them here.

## Code ownership

The TypeScript layer discovers projects, loads project-local compilers and Tailwind, and prepares planner inputs. The NAPI adapter dispatches to Rust capability crates. TypeScript then renders previews or applies transactional writes.

| Area                                            | Start here                                                                   |
| ----------------------------------------------- | ---------------------------------------------------------------------------- |
| CLI arguments and output                        | `src/bin.ts`                                                                 |
| Public API, option validation, orchestration    | `src/index.ts`, `src/types.ts`                                               |
| Package/workspace discovery and Git ignores     | `src/discovery.ts`                                                           |
| Tailwind entries, themes, shared-entry proofs   | `src/tailwind.ts`, `src/plan/entry.ts`                                       |
| Candidate validation, fonts, media definitions  | `src/plan/canonicalize.ts`, `src/plan/fonts.ts`, `src/plan/media.ts`         |
| HTML and Vue planning                           | `src/plan/html.ts`, `src/plan/vue.ts`                                        |
| HTML, preprocessor, and Vue parsing             | `src/parser/html.ts`, `src/parser/style-compiler.ts`, `src/parser/vue.ts`    |
| Previews, snapshots, transactional writes       | `src/util/diff.ts`, `src/util/shared.ts`, `src/util/write.ts`                |
| Native loading and NAPI dispatch                | `src/native.ts`, `crates/tw_migrate/src/lib.rs`                              |
| CSS selectors, utilities, themes, animations    | `crates/tw_migrate_css/src/`                                                 |
| JSX relationships and JS/HTML consumer rewrites | `crates/tw_migrate_source/src/jsx/`, `crates/tw_migrate_source/src/rewrite/` |
| Batch planning, source maps, Vue finishing      | `crates/tw_migrate_planner/src/`                                             |
| Typed failures and recoverability               | `crates/tw_migrate_error/src/lib.rs`                                         |
| API and byte-exact tests                        | `test/`                                                                      |
| Installed CLI behavior                          | `crates/snapshots/README.md`, `crates/snapshots/fixtures/`                   |
| Browser harness                                 | `ecosystem-ci/run.ts`, `ecosystem-ci/lifecycle.ts`, `ecosystem-ci/oracle.ts` |
| Package staging and local registry              | `ecosystem-ci/packages.ts`, `ecosystem-ci/registry.ts`, `npm/*`              |

## Safety and scope

- The CLI writes by default; the public API previews by default. Use `--dry-run` for local experiments. Dry runs must not modify files.
- Treat source changes during planning/writing, plan collisions, and write failures as fatal. `--force` may skip recoverable package input failures only.
- Reject symlink migration targets, preserve permissions, and keep writes transactional with rollback after partial failure.
- Preserve byte offsets and untouched source bytes. Map compiled preprocessor spans back to authored files before editing.
- Load Sass, Less, and Vue compilers from the target project. Do not fall back to repository dependencies.
- Retain unsupported or ambiguous rules with warnings instead of guessing. Compile generated candidates against the project's Tailwind entry.
- Support Tailwind v4 import-based entries. Direct `@tailwind` directives are outside the supported scope.
- Preserve external stylesheet imports without fetching them. Missing local imports must still fail.
- Use `zod/mini` at external-input boundaries without coercion. Reject unknown public API option keys, preserve authored strings and defaults, and keep public types independent of schema implementation details.

## Working on changes

- Identify the owning layer and trace shared callers before editing. Keep unrelated fixtures, snapshots, and generated files untouched.
- Choose focused checks from [CONTRIBUTING.md](CONTRIBUTING.md#validation), then run the owning suite. For CLI-visible changes, test the installed package rather than relying only on source-level tests.
- Read the [snapshot guide](crates/snapshots/README.md) before editing the runner or fixtures. Keep workspaces outside the repository to prevent dependency leakage, and do not broaden normalization to hide product-visible differences.
- Keep ecosystem assertions in the existing lifecycle/browser tests; do not recreate the deleted standalone harness unit suite. Preserve exact reports/bytes, rendering probes, causal witnesses, and selection before artifact preparation.
- Update [warning documentation](docs/warnings.md) and the planner's `WARNING_CODES` list together. `warning_codes_are_pinned_to_the_docs` checks both the table and emitted codes.
- Published JavaScript comes from `dist/`; local tests run `src/` with a debug addon. `src/native.ts` checks the local addon before the installed platform package. Do not commit generated `.node` files.

## Design references

- [Domain vocabulary](docs/CONCEPTS.md)
- [Core migration](rfcs/css-to-tailwind-migration-cli.md) and [batch/workspace migration](rfcs/batch-css-migration.md)
- [Preprocessors and HTML](rfcs/preprocessor-and-html-migration.md), [Vue SFCs](rfcs/vue-sfc-migration.md), and [class expressions](rfcs/dynamic-class-expression-migration.md)
- [Media extraction](rfcs/media-query-definition-extraction.md) and [candidate canonicalization](rfcs/tailwind-candidate-canonicalization.md)
- [Browser ecosystem contracts](rfcs/browser-ecosystem-e2e.md)

<!--VITE PLUS START-->

# Using Vite+, the Unified Toolchain for the Web

This project is using Vite+, a unified toolchain built on top of Vite, Rolldown, Vitest, tsdown, Oxlint, Oxfmt, and Vite Task. Vite+ wraps runtime management, package management, and frontend tooling in a single global CLI called `vp`. Vite+ is distinct from Vite, and it invokes Vite through `vp dev` and `vp build`. Run `vp help` to print a list of commands and `vp <command> --help` for information about a specific command.

Docs are local at `node_modules/vite-plus/docs` or online at https://viteplus.dev/guide/.

## Built-in Commands vs Scripts

`vp <name>` runs a built-in command. `vp run <name>` runs a `package.json` script or a `vite.config.ts` task. Scripts cannot overwrite built-ins, so `vp dev` and `vp run dev` may do different things. Check `package.json` and `vite.config.ts` first, and run `vp run <name>` when the project defines a script or task with that name.

## Tool Versions

Run `vp toolchain` to show versions and relationships in the active Vite+
release. Add a tool name to select part of the graph. For example, run
`vp toolchain vite`. Use `--global` to ignore the local `vite-plus` package. Use
`vp why <package>` to show the package-manager dependency graph.

## Review Checklist

- [ ] Run `vp install` after pulling remote changes and before getting started.
- [ ] Run `vp check` and `vp test` to format, lint, type check and test changes.
- [ ] Check if there are `vite.config.ts` tasks or `package.json` scripts necessary for validation, run via `vp run <script>`.
- [ ] If setup, runtime, or package-manager behavior looks wrong, run `vp env doctor` and include its output when asking for help.

<!--VITE PLUS END-->
