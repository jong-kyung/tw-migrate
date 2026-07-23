# AI Agent Guidelines for tw-migrate

This document helps AI coding assistants work on the **tw-migrate** repository. It is repository-specific; `CLAUDE.md` points here for compatibility.

Use this file to find the owning layer and choose focused validation. Prefer the implementation, RFCs, and test documentation over duplicating detailed behavior here.

## Project Overview

`tw-migrate` previews and applies migrations from static React, Next.js, and HTML stylesheet usage to Tailwind CSS v4 utilities. It supports CSS, SCSS, Sass, and Less, with conservative retention when a rewrite cannot be proven safe.

### Key Technologies

- **JavaScript / Node ESM**: CLI parsing, project and workspace discovery, preprocessors, HTML analysis, packaging, transactional writes, and the public `migrate()` API.
- **Rust 2024**: CSS parsing, utility generation, selector and JSX relationship analysis, rewrite planning, and source-map decoding.
- **NAPI-RS**: exposes the Rust planner to `native.js` and the JavaScript orchestration layer.
- **pnpm workspaces**: manages the root package and platform-specific native packages under `npm/*`.
- **Insta**: stores packaged CLI snapshots under `crates/snapshots/snapshots/`.

## Architecture

High-signal repository map:

```text
tw-migrate/
├── bin/tw-migrate.js          # Published CLI entrypoint and argument parsing
├── index.js                   # Public migrate() API and migration orchestration
├── index.d.ts                 # Public JavaScript API types
├── native.js                  # Native addon resolution and NAPI exports
├── html.js                    # HTML parsing and byte-offset extraction
├── style-compiler.js          # Project-local Sass/Less loading and source maps
├── crates/tw_migrate/         # Rust planner and NAPI addon
│   └── src/
│       ├── planner.rs         # Single/batch planning entrypoints
│       ├── css_plan.rs        # CSS rule planning
│       ├── js_rewrite.rs      # JS/TS rewrite planning
│       ├── jsx_graph.rs       # JSX relationship proofs
│       ├── html_rewrite.rs    # HTML rewrite planning
│       ├── utilities.rs       # CSS-to-Tailwind utility mapping
│       ├── arbitrary.rs       # Arbitrary value encoding
│       ├── at_rules.rs        # Conditional at-rule handling
│       ├── animations.rs      # Animation/keyframe migration
│       └── theme.rs           # Tailwind theme matching
├── crates/snapshots/          # Packaged CLI E2E runner, fixtures, and snapshots
├── test/migrate.test.js       # Public API, internal, and byte-exact Node tests
├── npm/*                      # Platform-specific published native packages
└── rfcs/                      # Design and supported-scope documents
```

## Runtime Flow

1. `bin/tw-migrate.js` parses CLI arguments and calls `migrate()`.
2. `index.js` discovers package/workspace inputs, reads source snapshots, compiles preprocessors, and prepares planner requests.
3. `style-compiler.js` loads Sass or Less from the target project, not from `tw-migrate` itself.
4. `native.js` loads the local addon or the installed platform package and invokes the Rust planner.
5. Rust analyzes CSS and source relationships, returning planned edits, candidates, warnings, and retained rules.
6. `index.js` verifies source integrity, renders the diff, and applies transactional writes only with `--write`.

## Where to Start

- **CLI flags, output, and exit behavior**: `bin/tw-migrate.js` and packaged snapshots.
- **Discovery, workspaces, Git ignore behavior, force handling, or writes**: `index.js`.
- **Public API shape**: `index.d.ts` and the top-level exports in `index.js`.
- **Sass, SCSS, Less, or source maps**: `style-compiler.js` and `crates/tw_migrate/src/lib.rs`.
- **HTML links, attributes, entities, or byte offsets**: `html.js` and `crates/tw_migrate/src/html_rewrite.rs`.
- **CSS parsing and migration decisions**: `crates/tw_migrate/src/planner.rs` and `css_plan.rs`.
- **JSX usage and selector relationships**: `js_rewrite.rs` and `jsx_graph.rs`.
- **Utility generation and value encoding**: `utilities.rs`, `arbitrary.rs`, `theme.rs`, `at_rules.rs`, and `animations.rs`.
- **Supported behavior and remaining scope**: `README.md` and `rfcs/`.
- **CLI-observable regressions**: `crates/snapshots/README.md` and `crates/snapshots/fixtures/`.

## Development Workflow

### Prerequisites

Use the repository-pinned tool versions when possible:

- **Node.js 24.18.0** from `.node-version`.
- **pnpm 11.15.1** from the `packageManager` field in `package.json`.
- **Rust 1.95.0 or newer**; CI builds with 1.95.0 and the workspace declares `rust-version = "1.95"`.
- **Git and npm**; runtime discovery uses Git and the packaged snapshot runner calls `npm pack` and `npm install` directly.
- **Platform native build tools** required by Rust and NAPI-RS: Xcode Command Line Tools on macOS, a C/C++ build toolchain on Linux, or Visual Studio Build Tools on Windows.

Initial setup requires npm and crates.io access unless dependencies are already cached. Packaged snapshots also run a fresh registry-backed install of pinned Tailwind, Sass, Less, and source-map packages in an isolated temporary directory.

Install `cargo-insta` only when reviewing or checking snapshot files:

```bash
cargo install cargo-insta --version 1.48.0 --locked
```

### Initial setup

```bash
pnpm install --frozen-lockfile
pnpm build:debug
node bin/tw-migrate.js --help
```

`pnpm build:debug` compiles the native addon for the current platform. Run it before invoking the CLI or Node tests directly. A complete setup check is:

```bash
pnpm test
```

No `.env` file or local service is required.

### Local build and CLI

```bash
pnpm build:debug
node bin/tw-migrate.js --help
node bin/tw-migrate.js path/to/Button.module.css
node bin/tw-migrate.js --workspaces --write
```

Preview is the default. Use `--write` only when a task explicitly requires filesystem changes.

### Validation

Choose checks by change type:

| Change type | Useful validation |
| --- | --- |
| Docs or agent guidance | `git diff --check -- <files>` and verify referenced paths/commands |
| Rust planner behavior | `cargo test` or a focused `cargo test <filter>` |
| JavaScript API/orchestration | `pnpm build:debug` followed by `node --test` or a focused Node test |
| CLI output or filesystem behavior | `pnpm test:snapshots` or a focused packaged snapshot case |
| Packaging/native loading | `pnpm build && pnpm artifacts` |
| Full local validation | `pnpm test && pnpm test:snapshots && git status --short` |

`pnpm test` runs the default Rust package, builds the debug addon, and runs the retained Node tests.

## Packaged CLI Snapshots

CLI-observable behavior belongs in `crates/snapshots/`. Read `crates/snapshots/README.md` before changing the runner or fixtures.

```bash
pnpm test:snapshots

# Focus one case after preparing release artifacts
pnpm snapshots:prepare
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
- Keep normalization limited to line endings, known roots, path separators inside known paths, and transaction tokens.
- Do not accept a snapshot until the expected exit status and workspace changes are correct.

The workspace `default-members` excludes `crates/snapshots`, so plain `cargo test` does not run package/network E2E tests. `cargo test --workspace` includes the snapshot crate and requires release artifacts.

## Safety Invariants

- Preserve preview-by-default behavior.
- Treat source changes during planning and writing as fatal integrity errors.
- `--force` may skip recoverable package input failures; it must not hide integrity, plan-collision, or write failures.
- Reject symlink migration targets and preserve source file permissions.
- Keep writes transactional and restore originals after partial failure.
- Preserve byte offsets and untouched bytes around JS, JSX, TS, TSX, and HTML edits.
- Load Sass and Less from the target project. Do not silently fall back to repository dependencies.
- Retain unsupported or ambiguous rules with a warning instead of producing an unsafe rewrite.

## Testing Strategy

- Put parser, planner, selector, and utility logic tests next to the Rust implementation.
- Keep structured public API, source-map, and byte-exact assertions in `test/migrate.test.js`.
- Put status/output/workspace behavior in packaged CLI snapshots.
- When changing public CLI behavior, update the fixture and snapshot together.
- Keep `crates/snapshots/coverage/*.toml` aligned with migrated legacy cases; `crates/snapshots/tests/inventory.rs` enforces the reconciliation.
- Use the smallest focused test while iterating, then run the owning suite before committing.

## Packaging Notes

- `pnpm build:debug` writes the local development addon used by Node tests.
- `pnpm build` creates the release addon; `pnpm artifacts` copies it into the matching `npm/<platform>/` package.
- `native.js` first checks for a local addon, then falls back to the installed platform package.
- Native `.node` files are generated and ignored. Do not commit them.
- Use `pnpm snapshots:prepare` before snapshot tests; it removes stale platform addons before rebuilding artifacts.

## Common Pitfalls

- Running `node --test` before building the debug addon.
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
- Public API: `index.d.ts`
- Core migration RFC: `rfcs/css-to-tailwind-migration-cli.md`
- Batch migration RFC: `rfcs/batch-css-migration.md`
- Preprocessor and HTML RFC: `rfcs/preprocessor-and-html-migration.md`
- Packaged snapshot workflow: `crates/snapshots/README.md`
- CI contract: `.github/workflows/ci.yml`
