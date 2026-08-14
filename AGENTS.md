# AI Agent Guidelines for tw-migrate

This document helps AI coding assistants work on the **tw-migrate** repository. It is repository-specific; `CLAUDE.md` points here for compatibility.

Use this file to find the owning layer and choose focused validation. Prefer the implementation, RFCs, and test documentation over duplicating detailed behavior here.

## Project Overview

`tw-migrate` previews and applies migrations from static React, Next.js, Vue 3 SFC, and HTML stylesheet usage to Tailwind CSS v4 utilities. It supports CSS, SCSS, Sass, and Less, with conservative retention when a rewrite cannot be proven safe.

### Key Technologies

- **TypeScript / Node ESM**: CLI parsing, project and workspace discovery, preprocessors, HTML analysis, packaging, transactional writes, and the public `migrate()` API.
- **Rust 2024**: CSS parsing, utility generation, selector and JSX relationship analysis, rewrite planning, and source-map decoding.
- **NAPI-RS**: exposes the thin Rust adapter in `crates/tw_migrate` to `src/native.ts`, and the adapter dispatches to the capability crates.
- **pnpm workspaces**: manages the root package and platform-specific native packages under `npm/*`.
- **Insta**: stores packaged CLI snapshots under `crates/snapshots/snapshots/`.

## Architecture

High-signal repository map:

```text
tw-migrate/
├── src/                       # Published TypeScript layer, bundled into dist/ by vp pack
│   ├── bin.ts                 # CLI entrypoint and argument parsing
│   ├── index.ts               # Public migrate() API, planning orchestration, plan merging
│   ├── types.ts               # Public API and shared internal type declarations
│   ├── discovery.ts           # Package/workspace discovery and Git-aware file scanning
│   ├── tailwind.ts            # Tailwind entry resolution, design-system and theme loading
│   ├── plan/
│   │   ├── html.ts            # HTML stylesheet-link contexts and consumer preparation
│   │   └── vue.ts             # Vue SFC planning, component graph, and shadow corpus
│   ├── util/
│   │   ├── diff.ts            # Unified diff rendering
│   │   ├── shared.ts          # Path classification, snapshots, and stylesheet reference helpers
│   │   └── write.ts           # Snapshot verification and transactional writes
│   ├── parser/
│   │   ├── html.ts            # HTML parsing and byte-offset extraction
│   │   ├── style-compiler.ts  # Project-local Sass/Less loading and source maps
│   │   └── vue.ts             # Project-local Vue compiler loading and SFC lowering
│   └── native.ts              # Native addon resolution and NAPI exports
├── crates/
│   ├── tw_migrate/            # NAPI annotations, error conversion, and capability dispatch
│   ├── tw_migrate_error/      # Typed migration failures and recoverability
│   ├── tw_migrate_css/        # CSS parsing, rule planning, utilities, media, and analysis
│   │   └── src/
│   │       ├── plan/          # Rule, selector, relationship, and shadow planning
│   │       ├── media/         # Media parsing, collection, and generated names
│   │       ├── utilities.rs   # CSS-to-Tailwind utility mapping and conflicts
│   │       ├── at_rules.rs    # Conditional and global at-rule handling
│   │       └── animations.rs  # Animation and keyframe migration
│   ├── tw_migrate_source/     # JS, JSX, HTML, and Vue-consumer analysis and rewriting
│   │   └── src/
│   │       ├── analysis/      # Source analysis used directly by NAPI
│   │       ├── jsx/           # JSX collection, linking, and relationship proof
│   │       └── rewrite/       # JavaScript and HTML consumer rewriting
│   ├── tw_migrate_planner/    # Batch orchestration, source maps, Vue finishing, and validation
│   │   └── src/
│   │       ├── batch.rs       # Batch planning entrypoint and conflict ordering
│   │       ├── stylesheet.rs  # Per-stylesheet planning coordination
│   │       ├── source_map.rs  # Source-map decoding and authored-span mapping
│   │       └── vue.rs         # Vue style-block masking, rebasing, and finishing
│   └── snapshots/             # Packaged CLI E2E runner, fixtures, and snapshots
├── ecosystem-ci/              # Browser E2E harness (TypeScript, run by Vitest)
│   ├── run.ts                 # Manifest validation and case selection CLI
│   ├── lifecycle.ts           # Per-case install, server, capture, and migration phases
│   ├── packages.ts            # Package staging, provenance, and installed-layout checks
│   ├── registry.ts            # Sealed local registry used by the harness
│   ├── oracle.ts              # Browser capture and computed-style comparison
│   ├── types.ts               # Manifest, provenance, and capture shapes
│   └── projects.json          # Controlled, smoke, and external case manifest
├── test/migrate.test.ts       # Public API, internal, and byte-exact Node tests
├── test/ecosystem-harness.test.ts  # Harness contracts, incl. asserted workflow content
├── vite.config.ts             # Vite+ fmt/lint/staged config and ignore scope
├── tsconfig.json              # Type-stripping-safe options for the .ts harness
├── npm/*                      # Platform-specific published native packages
└── rfcs/                      # Design and supported-scope documents
```

## Runtime Flow

1. `src/bin.ts` parses CLI arguments and calls `migrate()`.
2. `src/index.ts` orchestrates discovery (`src/discovery.ts`), source snapshots, preprocessor compilation, and planner request preparation (`src/plan/html.ts`, `src/plan/vue.ts`, `src/tailwind.ts`).
3. `src/parser/style-compiler.ts` loads Sass or Less from the target project, not from `tw-migrate` itself.
4. `src/native.ts` loads the local addon or the installed platform package and invokes the NAPI adapter in `crates/tw_migrate`.
5. The adapter dispatches CSS operations to `tw_migrate_css`, source operations to `tw_migrate_source`, and batch planning and source-map decoding to `tw_migrate_planner`. Rust returns planned edits, candidates, warnings, and retained rules.
6. `src/index.ts` verifies source integrity, renders the diff (`src/util/diff.ts`), and applies transactional writes (`src/util/write.ts`) unless `--dry-run` is passed.

## Where to Start

- **CLI flags, output, and exit behavior**: `src/bin.ts` and packaged snapshots.
- **Discovery, workspaces, or Git ignore behavior**: `src/discovery.ts`.
- **Force handling and plan orchestration**: `src/index.ts`.
- **Transactional writes and snapshot verification**: `src/util/write.ts`.
- **Public API shape**: the exported types and top-level exports in `src/index.ts` (published as generated `dist/index.d.ts`).
- **NAPI exports and native error conversion**: `src/native.ts` and `crates/tw_migrate/src/lib.rs`.
- **Sass, SCSS, Less, or source maps**: `src/parser/style-compiler.ts`, `crates/tw_migrate_planner/src/stylesheet.rs`, and `crates/tw_migrate_planner/src/source_map.rs`.
- **HTML links, attributes, entities, or byte offsets**: `src/parser/html.ts`, `src/plan/html.ts`, and `crates/tw_migrate_source/src/rewrite/html.rs`.
- **Vue SFC blocks, template class sites, or scoped retention**: `src/parser/vue.ts`, `src/plan/vue.ts`, `crates/tw_migrate_source/src/rewrite/html.rs`, and `crates/tw_migrate_planner/src/vue.rs`.
- **CSS parsing and migration decisions**: `crates/tw_migrate_css/src/plan/` and `crates/tw_migrate_planner/src/stylesheet.rs`.
- **JSX usage and selector relationships**: `crates/tw_migrate_source/src/rewrite/js/` and `crates/tw_migrate_source/src/jsx/`.
- **Utility generation and value encoding**: `crates/tw_migrate_css/src/utilities.rs`, `crates/tw_migrate_css/src/arbitrary.rs`, `crates/tw_migrate_css/src/theme.rs`, `crates/tw_migrate_css/src/at_rules.rs`, and `crates/tw_migrate_css/src/animations.rs`.
- **Typed Rust errors and recoverability**: `crates/tw_migrate_error/src/lib.rs`.
- **Supported behavior and remaining scope**: `README.md` and `rfcs/`.
- **CLI-observable regressions**: `crates/snapshots/README.md` and `crates/snapshots/fixtures/`.

## Development Workflow

### Prerequisites

Use the repository-pinned tool versions when possible:

- **Node.js 22.23.2** from `.node-version`; the repository targets `>=22.23.2` so Node runs the `.ts` harness sources directly through type stripping.
- **Vite+ (`vp`)** as the toolchain entrypoint; it resolves and downloads the pinned package manager itself.
- **pnpm 11.20.0** from the `packageManager` field in `package.json`; `vp` runs it, and the `pnpm` shim stays available for scripts that call it directly.
- **Rust 1.97.1** from `rust-toolchain.toml`; CI uses the same version, while the workspace minimum remains `rust-version = "1.95"`.
- **Git and npm**; runtime discovery uses Git and the packaged snapshot runner calls `npm pack` and `npm install` directly.
- **Platform native build tools** required by Rust and NAPI-RS: Xcode Command Line Tools on macOS, a C/C++ build toolchain on Linux, or Visual Studio Build Tools on Windows.

Initial setup requires npm and crates.io access unless dependencies are already cached. Packaged snapshots also run a fresh registry-backed install of pinned Tailwind, Sass, Less, and source-map packages in an isolated temporary directory.

Install `cargo-insta` only when reviewing or checking snapshot files:

```bash
cargo install cargo-insta --version 1.48.0 --locked
```

### Initial setup

```bash
vp install --frozen-lockfile
vp run build:debug
node src/bin.ts --help
```

`vp run build:debug` compiles the native addon for the current platform. Run it before invoking the CLI or Node tests directly. A complete setup check is:

```bash
vp run test
```

No `.env` file or local service is required.

### Local build and CLI

```bash
vp run build:debug
node src/bin.ts --help
node src/bin.ts path/to/Button.module.css
node src/bin.ts --workspaces
```

Write is the default. Use `--dry-run` while iterating unless a task explicitly requires filesystem changes.

### Validation

Choose checks by change type:

| Change type                       | Useful validation                                                  |
| --------------------------------- | ------------------------------------------------------------------ |
| Docs or agent guidance            | `git diff --check -- <files>` and verify referenced paths/commands |
| Rust planner behavior             | `cargo test` or a focused `cargo test <filter>`                    |
| JavaScript API/orchestration      | `vp run build:debug` followed by `vp test` or `vp test <file>`     |
| CLI output or filesystem behavior | `vp run test:snapshots` or a focused packaged snapshot case        |
| Packaging/native loading          | `vp run build && vp run artifacts`                                 |
| Full local validation             | `vp check && vp run test && vp run test:snapshots`                 |

`vp run test` runs the default Rust package, builds the debug addon, and runs the Vite+ tests. `vp check` covers formatting, linting, and type checking.

## Packaged CLI Snapshots

CLI-observable behavior belongs in `crates/snapshots/`. Read `crates/snapshots/README.md` before changing the runner or fixtures.

```bash
vp run test:snapshots

# Focus one case after preparing release artifacts
vp run snapshots:prepare
cargo test -p tw-migrate-snapshots safety_missing_sass

# Review and check snapshot hygiene
cargo insta test -p tw-migrate-snapshots --review
cargo insta test --check --unreferenced reject -p tw-migrate-snapshots
```

Important properties:

- The suite packs and installs the root package plus the current platform package, then executes the installed CLI.
- It performs one registry-backed npm install per test process.
- Workspaces live under the OS temporary directory, outside the repository, to prevent dependency and project-discovery leakage.
- Snapshots share one Linux/macOS/Windows baseline and record status, stdout, stderr, and per-step workspace deltas.
- Keep normalization limited to line endings, known roots, path separators inside known paths, transaction tokens, and the Windows-only strip of the workspace prefix in Sass root-stylesheet traces.
- Do not accept a snapshot until the expected exit status and workspace changes are correct.

The workspace `default-members` excludes `crates/snapshots`, so plain `cargo test` does not run package/network E2E tests. `cargo test --workspace` includes the snapshot crate and requires release artifacts.

## Safety Invariants

- `--dry-run` must never modify the filesystem.
- Treat source changes during planning and writing as fatal integrity errors.
- `--force` may skip recoverable package input failures; it must not hide integrity, plan-collision, or write failures.
- Reject symlink migration targets and preserve source file permissions.
- Keep writes transactional and restore originals after partial failure.
- Preserve byte offsets and untouched bytes around JS, JSX, TS, TSX, and HTML edits.
- Load Sass and Less from the target project. Do not silently fall back to repository dependencies.
- Retain unsupported or ambiguous rules with a warning instead of producing an unsafe rewrite.

## Testing Strategy

- Put parser, planner, selector, and utility logic tests next to the Rust implementation.
- Keep structured public API, source-map, and byte-exact assertions in `test/migrate.test.ts`.
- Put status/output/workspace behavior in packaged CLI snapshots.
- When changing public CLI behavior, update the fixture and snapshot together.
- Use the smallest focused test while iterating, then run the owning suite before committing.

## Packaging Notes

- `vp run build:debug` writes the local development addon used by Node tests.
- `vp run build` bundles `src/` into `dist/` with `vp pack` and creates the release addon; `vp run artifacts` copies the addon into the matching `npm/<platform>/` package.
- Published JavaScript is the `dist/` bundle; tests and local CLI runs execute `src/` directly.
- `src/native.ts` first checks for a local addon, then falls back to the installed platform package.
- Native `.node` files are generated and ignored. Do not commit them.
- Use `vp run snapshots:prepare` before snapshot tests; it removes stale platform addons before rebuilding artifacts.

## Common Pitfalls

- Running `vp test` before building the debug addon.
- Passing `--help` through `vp node <script>`; `vp` consumes it and prints its own help. Other arguments pass through, so run `node <script> --help` when you want the script's usage.
- Treating compiled preprocessor offsets as authored-file offsets without source-map validation.
- Adding broad output normalization that hides product-visible differences such as escaped Tailwind candidates.
- Moving snapshot workspaces under the repository, where root dependencies can invalidate missing-compiler tests.
- Replacing retained rules with speculative conversions when relationships or source origins are ambiguous.
- Running `cargo test --workspace` and unintentionally triggering the packaged snapshot crate without prerequisites.

## AI Assistant Tips

- Identify the owning layer before editing: CLI, JavaScript orchestration, preprocessor/HTML parsing, Rust planning, packaging, or snapshots.
- Trace shared planner and migration entrypoints before changing them.
- Keep changes scoped and leave unrelated generated files, fixtures, and snapshots untouched.
- Verify the real installed CLI for user-visible behavior; source-level tests alone do not cover packaging boundaries.

## References

- Product overview and warning codes: `README.md`
- Public API: `src/index.ts` exported types (published as `dist/index.d.ts`)
- Core migration RFC: `rfcs/css-to-tailwind-migration-cli.md`
- Batch migration RFC: `rfcs/batch-css-migration.md`
- Preprocessor and HTML RFC: `rfcs/preprocessor-and-html-migration.md`
- Packaged snapshot workflow: `crates/snapshots/README.md`
- CI contract: `.github/workflows/ci.yml` (cargo-shear, unit, typecheck, packaged snapshots) and `.github/workflows/ecosystem.yml` (label-gated browser E2E)

<!--VITE PLUS START-->

# Using Vite+, the Unified Toolchain for the Web

This project is using Vite+, a unified toolchain built on top of Vite, Rolldown, Vitest, tsdown, Oxlint, Oxfmt, and Vite Task. Vite+ wraps runtime management, package management, and frontend tooling in a single global CLI called `vp`. Vite+ is distinct from Vite, and it invokes Vite through `vp dev` and `vp build`. Run `vp help` to print a list of commands and `vp <command> --help` for information about a specific command.

Docs are local at `node_modules/vite-plus/docs` or online at https://viteplus.dev/guide/.

## Built-in Commands vs Scripts

`vp <name>` runs a built-in command. `vp run <name>` runs a `package.json` script or a `vite.config.ts` task. Scripts cannot overwrite built-ins, so `vp dev` and `vp run dev` may do different things. Check `package.json` and `vite.config.ts` first, and run `vp run <name>` when the project defines a script or task with that name.

## Review Checklist

- [ ] Run `vp install` after pulling remote changes and before getting started.
- [ ] Run `vp check` and `vp test` to format, lint, type check and test changes.
- [ ] Check if there are `vite.config.ts` tasks or `package.json` scripts necessary for validation, run via `vp run <script>`.
- [ ] If setup, runtime, or package-manager behavior looks wrong, run `vp env doctor` and include its output when asking for help.

<!--VITE PLUS END-->
